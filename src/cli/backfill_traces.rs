//! `tidx backfill-traces` — fetch `debug_traceTransaction` for txs in a block
//! range and persist the flattened call frames to `internal_txs`.
//!
//! Use after the schema migration has shipped (so `internal_txs` exists) to
//! catch up history. The realtime engine writes traces inline when started
//! with tracing enabled; this CLI fills in the past.
//!
//! Resumable: by default skips txs that already have at least one
//! internal_txs row. Pass `--all` to re-trace everything (e.g. after a tracer
//! bug fix).

use anyhow::{Result, anyhow};
use clap::Args as ClapArgs;
use std::path::PathBuf;
use tracing::{info, warn};

use tidx::config::Config;
use tidx::db;
use tidx::sync::ch_sink::ClickHouseSink;
use tidx::sync::fetcher::RpcClient;
use tidx::sync::sink::SinkSet;
use tidx::sync::trace::fetch_and_flatten_traces;
use tidx::types::TxRow;

#[derive(ClapArgs)]
pub struct Args {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    pub config: PathBuf,

    /// Chain ID (uses first chain if not specified)
    #[arg(long)]
    pub chain_id: Option<u64>,

    /// First L2 block number to scan (inclusive)
    #[arg(long)]
    pub from: u64,

    /// Last L2 block number to scan (inclusive)
    #[arg(long)]
    pub to: u64,

    /// Number of blocks per processing chunk. Each chunk fires one
    /// debug_traceTransaction RPC per tx in the range, capped by the RPC
    /// client's internal concurrency limit.
    #[arg(long, default_value = "500")]
    pub batch_size: u64,

    /// Re-trace txs that already have internal_txs rows. Default skips them.
    #[arg(long)]
    pub all: bool,
}

pub async fn run(args: Args) -> Result<()> {
    if args.from > args.to {
        return Err(anyhow!(
            "--from ({}) must be <= --to ({})",
            args.from,
            args.to
        ));
    }
    if args.batch_size == 0 {
        return Err(anyhow!("--batch-size must be greater than zero"));
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

    let mut sinks = SinkSet::new(pool.clone());
    if let Some(ch_config) = &chain.clickhouse {
        if ch_config.enabled {
            let database = ch_config
                .database
                .clone()
                .unwrap_or_else(|| format!("tidx_{}", chain.chain_id));
            let password = ch_config.resolved_password()?;
            let ch_sink = ClickHouseSink::new(
                &ch_config.url,
                &database,
                ch_config.user.as_deref(),
                password.as_deref(),
            )?;
            ch_sink.ensure_schema().await?;
            sinks = sinks.with_clickhouse(ch_sink);
        }
    }

    let rpc = RpcClient::new(&chain.rpc_url);

    info!(
        chain = %chain.name,
        chain_id = chain.chain_id,
        from = args.from,
        to = args.to,
        batch_size = args.batch_size,
        all = args.all,
        "Starting internal_txs backfill"
    );

    let mut current = args.from;
    let mut scanned_txs = 0_u64;
    let mut written_rows = 0_u64;

    while current <= args.to {
        let batch_end = (current + args.batch_size - 1).min(args.to);
        let txs = load_txs_in_range(&pool, current as i64, batch_end as i64, !args.all).await?;
        if txs.is_empty() {
            current = batch_end + 1;
            continue;
        }

        let trace_rows = fetch_and_flatten_traces(&rpc, &txs).await?;
        if !trace_rows.is_empty() {
            sinks.write_internal_txs(&trace_rows).await?;
            written_rows += trace_rows.len() as u64;
        }

        scanned_txs += txs.len() as u64;
        info!(
            from = current,
            to = batch_end,
            txs = txs.len(),
            internal_rows = trace_rows.len(),
            scanned_txs,
            written_rows,
            "Backfilled trace batch"
        );

        if batch_end == u64::MAX {
            break;
        }
        current = batch_end + 1;
    }

    if scanned_txs == 0 {
        warn!("No txs found in the requested range — nothing to backfill");
    }
    info!(
        scanned_txs,
        written_rows, "internal_txs backfill complete"
    );
    Ok(())
}

/// Load txs in `[from, to]`. When `skip_existing` is true, only returns txs
/// that don't yet have any `internal_txs` rows (resumable backfill).
async fn load_txs_in_range(
    pool: &db::Pool,
    from: i64,
    to: i64,
    skip_existing: bool,
) -> Result<Vec<TxRow>> {
    let conn = pool.get().await?;
    let sql = if skip_existing {
        // Anti-join against internal_txs: skip txs that already have at least
        // one nested-call row. Index on internal_txs.tx_hash makes this cheap.
        r#"SELECT t.block_num, t.block_timestamp, t.idx, t.hash, t.type, t."from", t."to",
                  t.value, t.input, t.gas_limit, t.max_fee_per_gas, t.max_priority_fee_per_gas,
                  t.gas_used, t.nonce_key, t.nonce, t.fee_token, t.fee_payer, t.calls,
                  t.call_count, t.valid_before, t.valid_after, t.signature_type, t.selector
           FROM txs t
           WHERE t.block_num BETWEEN $1 AND $2
             AND NOT EXISTS (
                 SELECT 1 FROM internal_txs i WHERE i.tx_hash = t.hash
             )
           ORDER BY t.block_num, t.idx"#
    } else {
        r#"SELECT block_num, block_timestamp, idx, hash, type, "from", "to",
                  value, input, gas_limit, max_fee_per_gas, max_priority_fee_per_gas,
                  gas_used, nonce_key, nonce, fee_token, fee_payer, calls,
                  call_count, valid_before, valid_after, signature_type, selector
           FROM txs
           WHERE block_num BETWEEN $1 AND $2
           ORDER BY block_num, idx"#
    };
    let rows = conn.query(sql, &[&from, &to]).await?;
    Ok(rows
        .iter()
        .map(|r| TxRow {
            block_num: r.get(0),
            block_timestamp: r.get(1),
            idx: r.get(2),
            hash: r.get(3),
            tx_type: r.get(4),
            from: r.get(5),
            to: r.get(6),
            value: r.get(7),
            input: r.get(8),
            gas_limit: r.get(9),
            max_fee_per_gas: r.get(10),
            max_priority_fee_per_gas: r.get(11),
            gas_used: r.get(12),
            nonce_key: r.get(13),
            nonce: r.get(14),
            fee_token: r.get(15),
            fee_payer: r.get(16),
            calls: r.get(17),
            call_count: r.get(18),
            valid_before: r.get(19),
            valid_after: r.get(20),
            signature_type: r.get(21),
            selector: r.get(22),
        })
        .collect())
}
