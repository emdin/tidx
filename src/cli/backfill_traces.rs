//! `tidx backfill-traces` — fetch `debug_traceTransaction` for txs in a block
//! range, persist the flattened call frames to `internal_txs` and record an
//! outcome per tx in `trace_outcomes`.
//!
//! Default selection is the same as the engine's repair loop: txs that were
//! never traced (no `trace_outcomes` row) or `failed` with attempts left.
//! Txs traced `ok`/`empty` are skipped, so the run is resumable and never
//! re-traces a tx just because it made no nested call.
//!
//! `--mark-existing` stamps an `ok` outcome (no RPC) on txs that already have
//! `internal_txs` rows but no outcome — history traced before
//! `trace_outcomes` existed — one chunk at a time, ahead of tracing that chunk.
//! Run it once over full history when upgrading, BEFORE starting the new
//! engine (whose repair loop would otherwise re-trace the last 100k blocks).
//!
//! `--all` re-traces every tx in range regardless of outcome (e.g. after a
//! tracer bug fix).

use anyhow::{Result, anyhow};
use clap::Args as ClapArgs;
use std::path::PathBuf;
use tracing::{info, warn};

use tidx::config::Config;
use tidx::db;
use tidx::sync::ch_sink::ClickHouseSink;
use tidx::sync::fetcher::RpcClient;
use tidx::sync::sink::SinkSet;
use tidx::sync::trace::{DEFAULT_TRACE_ATTEMPTS, TraceStatus, trace_txs};
use tidx::sync::writer::{load_txs_for_trace_repair, load_txs_in_range, mark_existing_traces};

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

    /// Re-trace every tx in range, ignoring recorded outcomes.
    #[arg(long)]
    pub all: bool,

    /// Before tracing, record `ok` outcomes (no RPC) for txs that already
    /// have internal_txs rows but no outcome row.
    #[arg(long)]
    pub mark_existing: bool,

    /// Skip `failed` txs whose cumulative attempts reached this many (default: no cap).
    #[arg(long, default_value_t = i32::MAX)]
    pub max_attempts: i32,
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
        mark_existing = args.mark_existing,
        "Starting internal_txs backfill"
    );

    let mut current = args.from;
    let mut scanned_txs = 0_u64;
    let mut written_rows = 0_u64;
    let mut failed_txs = 0_u64;
    let mut marked_txs = 0_u64;

    while current <= args.to {
        let batch_end = (current + args.batch_size - 1).min(args.to);
        if args.mark_existing {
            marked_txs += mark_existing_traces(&pool, current as i64, batch_end as i64).await?;
        }
        let txs = if args.all {
            load_txs_in_range(&pool, current as i64, batch_end as i64).await?
        } else {
            // retry_after 0: an explicit operator run retries failures now
            load_txs_for_trace_repair(&pool, current as i64, batch_end as i64, args.max_attempts, 0, i64::MAX).await?
        };
        if txs.is_empty() {
            current = batch_end + 1;
            continue;
        }

        let batch = trace_txs(&rpc, &txs, DEFAULT_TRACE_ATTEMPTS).await;
        let failed = batch
            .outcomes
            .iter()
            .filter(|o| o.status == TraceStatus::Failed)
            .count() as u64;
        sinks.write_traces(&batch).await?;
        written_rows += batch.rows.len() as u64;
        failed_txs += failed;
        scanned_txs += txs.len() as u64;
        info!(
            from = current,
            to = batch_end,
            txs = txs.len(),
            internal_rows = batch.rows.len(),
            failed,
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
        warn!("No txs needed tracing in the requested range");
    }
    info!(
        scanned_txs,
        written_rows,
        failed_txs,
        marked_txs,
        "internal_txs backfill complete (failed txs stay 'failed' for the next pass)"
    );
    Ok(())
}
