//! `tidx backfill-kaspa-tx-index` — populate `kaspa_tx_index` by walking a
//! Kaspa wRPC node's virtual chain (chain blocks + mergeset blocks) and
//! upserting every tx's `(txid, block_hash, daa_score)`.
//!
//! Why this exists: kaspad's native gRPC/wRPC does not expose a "get tx by
//! id" method. Its `getBlock(hash, true)` returns tx inputs as
//! `previousOutpoint` references with NO resolved amounts. To compute
//! `fee = sum(inputs) - sum(outputs)` locally we need a txid → block_hash
//! mapping, so we can then `getBlock` on that hash and read the referenced
//! output amount. This CLI populates that mapping for a historical window;
//! realtime sync keeps it fresh going forward.
//!
//! Idempotent: `ON CONFLICT (txid) DO NOTHING`. Safe to re-run over any
//! overlapping window.

use anyhow::{Context, Result, anyhow};
use clap::Args as ClapArgs;
use kaspa_rpc_core::{RpcBlock, RpcHash, api::rpc::RpcApi};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};

use tidx::config::Config;
use tidx::db;
use tidx::kaspa::client::connect_borsh_wrpc;
use tidx::kaspa::tx_index::{self, TxIndexRow};

#[derive(ClapArgs)]
pub struct Args {
    /// Path to config file (used for DB connection only)
    #[arg(short, long, default_value = "config.toml")]
    pub config: PathBuf,

    /// Chain ID (uses first chain if not specified)
    #[arg(long)]
    pub chain_id: Option<u64>,

    /// Kaspa wRPC URL (Borsh) to walk. Examples:
    ///   - ws://127.0.0.1:17110 (local kaspad-mainnet)
    ///   - wss://archival.kaspa.ws
    #[arg(long)]
    pub rpc: String,

    /// Explicit chain block hash to start walking from. Defaults to the
    /// wRPC node's current pruning_point — i.e., the earliest block still
    /// retained.
    #[arg(long)]
    pub start_hash: Option<String>,

    /// Stop walking once we've processed this many chain blocks.
    #[arg(long)]
    pub max_blocks: Option<u64>,

    /// Rows per UPSERT batch. Larger = fewer PG round-trips but more work
    /// wasted on a crash. Default is a happy middle.
    #[arg(long, default_value = "500")]
    pub batch_size: usize,

    /// Don't write; only count rows that would be inserted.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Default, Debug)]
struct WalkStats {
    chain_blocks: u64,
    mergeset_blocks: u64,
    txs_seen: u64,
    rows_inserted: u64,
}

pub async fn run(args: Args) -> Result<()> {
    if args.batch_size == 0 {
        return Err(anyhow!("--batch-size must be > 0"));
    }

    let cfg = Config::load(&args.config)?;
    let chain = if let Some(id) = args.chain_id {
        cfg.chains
            .iter()
            .find(|c| c.chain_id == id)
            .ok_or_else(|| anyhow!("Chain ID {} not found in config", id))?
    } else {
        cfg.chains
            .first()
            .ok_or_else(|| anyhow!("No chains configured"))?
    };

    let pool: Option<db::Pool> = if args.dry_run {
        None
    } else {
        let pg_url = chain.resolved_pg_url()?;
        let pool = db::create_pool(&pg_url).await?;
        db::run_migrations(&pool).await?;
        Some(pool)
    };

    info!(rpc = %args.rpc, "Connecting to Kaspa wRPC source");
    let client = connect_borsh_wrpc(&args.rpc).await?;

    let server_info = client
        .get_server_info()
        .await
        .context("get_server_info on backfill source")?;
    let dag = client
        .get_block_dag_info()
        .await
        .context("get_block_dag_info on backfill source")?;
    info!(
        version = %server_info.server_version,
        synced = server_info.is_synced,
        pruning_point = %dag.pruning_point_hash,
        sink = %dag.sink,
        virtual_daa = dag.virtual_daa_score,
        "Backfill source ready"
    );

    let start_hash: RpcHash = match &args.start_hash {
        Some(s) => s
            .parse()
            .with_context(|| format!("--start-hash must be a 64-hex Kaspa block hash: {s}"))?,
        None => dag.pruning_point_hash,
    };
    info!(start_hash = %start_hash, "Walk start point");

    if args.dry_run {
        warn!("DRY RUN — counts only, no writes to the database.");
    }

    let stats = walk_and_index(
        &client,
        pool.as_ref(),
        start_hash,
        args.max_blocks,
        args.batch_size,
        args.dry_run,
    )
    .await?;

    info!(
        chain_blocks = stats.chain_blocks,
        mergeset_blocks = stats.mergeset_blocks,
        txs_seen = stats.txs_seen,
        rows_inserted = stats.rows_inserted,
        "Backfill complete"
    );
    Ok(())
}

/// Walk the virtual chain forward from `start_hash`, fetch every chain
/// block and every mergeset block, and upsert every tx we find into
/// `kaspa_tx_index`. Blocks are visited at most once per invocation via
/// a `HashSet` on their hash (chain blocks accept mergeset blocks, some
/// mergeset blocks are shared across acceptance points — dedup avoids
/// redundant PG work).
async fn walk_and_index(
    client: &kaspa_wrpc_client::KaspaRpcClient,
    pool: Option<&db::Pool>,
    start_hash: RpcHash,
    max_blocks: Option<u64>,
    batch_size: usize,
    dry_run: bool,
) -> Result<WalkStats> {
    let mut stats = WalkStats::default();
    let mut cursor = start_hash;
    let mut visited: HashSet<[u8; 32]> = HashSet::new();
    let mut pending: Vec<TxIndexRow> = Vec::with_capacity(batch_size);
    let mut last_progress_log = std::time::Instant::now();

    loop {
        let resp = client
            .get_virtual_chain_from_block(cursor, false, None)
            .await
            .with_context(|| format!("get_virtual_chain_from_block from {cursor}"))?;

        if resp.added_chain_block_hashes.is_empty() {
            break;
        }

        for chain_block_hash in &resp.added_chain_block_hashes {
            stats.chain_blocks += 1;

            // The chain block itself.
            index_block(client, *chain_block_hash, &mut visited, &mut pending, &mut stats).await?;

            // And its mergeset (blues + reds). Most Kaspa txs live in
            // mergeset blocks, not chain blocks — skipping them would
            // leave most of the index empty.
            let chain_block = client
                .get_block(*chain_block_hash, false)
                .await
                .with_context(|| format!("get_block (mergeset resolution) {chain_block_hash}"))?;
            if let Some(verbose) = &chain_block.verbose_data {
                for mh in verbose
                    .merge_set_blues_hashes
                    .iter()
                    .chain(verbose.merge_set_reds_hashes.iter())
                    .copied()
                {
                    stats.mergeset_blocks += 1;
                    index_block(client, mh, &mut visited, &mut pending, &mut stats).await?;
                }
            }

            // Flush batches as they fill.
            if pending.len() >= batch_size {
                flush(pool, dry_run, &mut pending, &mut stats).await?;
            }

            cursor = *chain_block_hash;
            if last_progress_log.elapsed() >= Duration::from_secs(20) {
                info!(
                    chain_blocks = stats.chain_blocks,
                    mergeset_blocks = stats.mergeset_blocks,
                    txs_seen = stats.txs_seen,
                    rows_inserted = stats.rows_inserted,
                    cursor = %chain_block_hash,
                    "progress"
                );
                last_progress_log = std::time::Instant::now();
            }
            if let Some(cap) = max_blocks {
                if stats.chain_blocks >= cap {
                    flush(pool, dry_run, &mut pending, &mut stats).await?;
                    return Ok(stats);
                }
            }
        }
    }

    // Final flush.
    flush(pool, dry_run, &mut pending, &mut stats).await?;
    Ok(stats)
}

/// Fetch one block (with transactions), extract rows, buffer them into
/// `pending`. Skips already-visited blocks (dedup within one walk).
async fn index_block(
    client: &kaspa_wrpc_client::KaspaRpcClient,
    hash: RpcHash,
    visited: &mut HashSet<[u8; 32]>,
    pending: &mut Vec<TxIndexRow>,
    stats: &mut WalkStats,
) -> Result<()> {
    let hash_bytes = hash.as_bytes();
    if !visited.insert(hash_bytes) {
        return Ok(());
    }
    let block: RpcBlock = client
        .get_block(hash, true)
        .await
        .with_context(|| format!("get_block {hash}"))?;
    let daa = i64::try_from(block.header.daa_score).ok();
    let rows = tx_index::extract_rows(hash_bytes, daa, &block.transactions);
    stats.txs_seen += rows.len() as u64;
    pending.extend(rows);
    Ok(())
}

/// Flush the pending batch. On dry-run we count-as-would-write; on real
/// runs we hit PG. `pending` is emptied.
async fn flush(
    pool: Option<&db::Pool>,
    dry_run: bool,
    pending: &mut Vec<TxIndexRow>,
    stats: &mut WalkStats,
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    if dry_run || pool.is_none() {
        // Best-effort counter: dry_run reports "seen" as an upper bound
        // for what *would* be written. Actual conflicts happen only on
        // real writes; count them as "rows_inserted" in the live path.
        pending.clear();
        return Ok(());
    }
    let n = tx_index::upsert_batch(pool.unwrap(), pending).await?;
    stats.rows_inserted += n;
    pending.clear();
    Ok(())
}
