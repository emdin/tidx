//! `tidx backfill-denorm` — populate the denormalized columns introduced for
//! phase 2 (`txs.selector`, `logs."from"`) for rows that pre-date the schema
//! migration.
//!
//! Idempotent: only updates rows where the new column is NULL. Chunked by
//! block_num so a single pass doesn't take a global table lock or generate a
//! gigabyte of WAL. Operator runs once after deploy; subsequent runs are no-ops
//! once everything is filled in.

use anyhow::{Result, anyhow};
use clap::Args as ClapArgs;
use std::path::PathBuf;
use tracing::info;

use tidx::config::Config;
use tidx::db;

#[derive(ClapArgs)]
pub struct Args {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    pub config: PathBuf,

    /// Chain ID (uses first chain if not specified)
    #[arg(long)]
    pub chain_id: Option<u64>,

    /// Number of blocks per chunk. Larger = fewer round-trips but longer per-
    /// chunk WAL pressure. 100k blocks ≈ ~100k tx updates on Igra.
    #[arg(long, default_value = "100000")]
    pub batch_size: i64,

    /// Skip the txs.selector backfill.
    #[arg(long)]
    pub skip_txs: bool,

    /// Skip the logs."from" backfill.
    #[arg(long)]
    pub skip_logs: bool,
}

pub async fn run(args: Args) -> Result<()> {
    if args.batch_size <= 0 {
        return Err(anyhow!("--batch-size must be positive"));
    }

    let config = Config::load(&args.config)?;
    let chain = if let Some(id) = args.chain_id {
        config
            .chains
            .iter()
            .find(|c| c.chain_id == id)
            .ok_or_else(|| anyhow!("Chain ID {} not found in config", id))?
    } else {
        config
            .chains
            .first()
            .ok_or_else(|| anyhow!("No chains configured"))?
    };

    let pg_url = chain.resolved_pg_url()?;
    let pool = db::create_pool(&pg_url).await?;

    if !args.skip_txs {
        backfill_txs_selector(&pool, args.batch_size).await?;
    }
    if !args.skip_logs {
        backfill_logs_from(&pool, args.batch_size).await?;
    }

    info!("backfill-denorm complete");
    Ok(())
}

async fn backfill_txs_selector(pool: &db::Pool, batch_size: i64) -> Result<()> {
    let conn = pool.get().await?;

    let row = conn
        .query_one(
            "SELECT COALESCE(MIN(block_num), 0), COALESCE(MAX(block_num), 0)
             FROM txs WHERE selector IS NULL AND octet_length(input) >= 4",
            &[],
        )
        .await?;
    let min_block: i64 = row.get(0);
    let max_block: i64 = row.get(1);

    if min_block == 0 && max_block == 0 {
        info!("txs.selector: no rows to backfill");
        return Ok(());
    }

    info!(
        min_block,
        max_block,
        batch_size,
        "txs.selector: starting backfill"
    );

    let mut current = min_block;
    let mut total_updated: u64 = 0;

    while current <= max_block {
        let chunk_end = (current + batch_size - 1).min(max_block);
        let updated = conn
            .execute(
                "UPDATE txs SET selector = substring(input, 1, 4)
                 WHERE selector IS NULL AND octet_length(input) >= 4
                   AND block_num BETWEEN $1 AND $2",
                &[&current, &chunk_end],
            )
            .await?;
        total_updated += updated;
        info!(
            from = current,
            to = chunk_end,
            updated,
            total = total_updated,
            "txs.selector: chunk done"
        );
        current = chunk_end + 1;
    }

    info!(total_updated, "txs.selector: backfill complete");
    Ok(())
}

async fn backfill_logs_from(pool: &db::Pool, batch_size: i64) -> Result<()> {
    let conn = pool.get().await?;

    let row = conn
        .query_one(
            "SELECT COALESCE(MIN(block_num), 0), COALESCE(MAX(block_num), 0)
             FROM logs WHERE \"from\" IS NULL",
            &[],
        )
        .await?;
    let min_block: i64 = row.get(0);
    let max_block: i64 = row.get(1);

    if min_block == 0 && max_block == 0 {
        info!("logs.\"from\": no rows to backfill");
        return Ok(());
    }

    info!(
        min_block,
        max_block,
        batch_size,
        "logs.\"from\": starting backfill"
    );

    let mut current = min_block;
    let mut total_updated: u64 = 0;

    while current <= max_block {
        let chunk_end = (current + batch_size - 1).min(max_block);
        let updated = conn
            .execute(
                "UPDATE logs SET \"from\" = txs.\"from\"
                 FROM txs
                 WHERE logs.block_num = txs.block_num
                   AND logs.tx_idx = txs.idx
                   AND logs.\"from\" IS NULL
                   AND logs.block_num BETWEEN $1 AND $2",
                &[&current, &chunk_end],
            )
            .await?;
        total_updated += updated;
        info!(
            from = current,
            to = chunk_end,
            updated,
            total = total_updated,
            "logs.\"from\": chunk done"
        );
        current = chunk_end + 1;
    }

    info!(total_updated, "logs.\"from\": backfill complete");
    Ok(())
}
