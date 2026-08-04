//! `tidx backfill-kaspa-tx-index` — populate `kaspa_tx_index` by walking a
//! Kaspa wRPC node's virtual chain (chain blocks + mergeset blocks) and
//! upserting every tx's `(txid, block_hash, daa_score)`.
//!
//! Why this exists: kaspad's RPC exposes `GetBlock(hash, true)` which
//! returns a tx with inputs as bare `previousOutpoint` refs (no resolved
//! amounts), and `GetUtxoReturnAddress(txid, daa)` which returns only one
//! address. Neither returns a confirmed tx's inputs with resolved amounts,
//! which is what `fee = sum(inputs) - sum(outputs)` needs. Our
//! `kaspa_tx_index` bridges the gap: (txid → block_hash) lets us walk
//! each input's prev tx via `getBlock` and read the output at the
//! referenced index locally.
//!
//! The walk is two-phase to cover kaspad's full retention window:
//!   1. Backward pass: from pruning_point through `selected_parent_hash`
//!      until we've covered `--walk-back-days` (default 30) or hit the
//!      retention floor.
//!   2. Forward pass: from pruning_point via `get_virtual_chain_from_block`
//!      up to the current sink. Plus `--follow` mode for realtime.
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

    /// After the initial walk catches up, keep polling kaspad for new
    /// chain blocks and index them as they arrive. Runs forever until
    /// killed. Use as a lightweight sidecar to the main indexer — cheap
    /// (a few block fetches every 10s in steady state) and decoupled
    /// from the sync hot path.
    #[arg(long)]
    pub follow: bool,

    /// In `--follow` mode: seconds between polls of `get_block_dag_info`
    /// once caught up.
    #[arg(long, default_value = "10")]
    pub follow_interval_secs: u64,

    /// How many days of history to walk BACKWARD from the pruning point
    /// via `selected_parent_hash` before the forward pass. Kaspad's
    /// `--retention-period-days` extends `GetBlock` beyond the pruning
    /// point; without this pass we'd only index the ~1-2 days between
    /// pruning_point and virtual sink. 0 = skip the backward pass.
    /// Set higher than kaspad's actual retention → walk stops naturally
    /// when kaspad returns "block not found."
    #[arg(long, default_value = "30.0")]
    pub walk_back_days: f64,
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

    let follow_interval = if args.follow {
        Some(Duration::from_secs(args.follow_interval_secs))
    } else {
        None
    };

    // Phase 1 — backward walk. Only relevant when the user asked for it and
    // when start_hash is at the pruning point (the natural place to walk
    // back from). If the user supplied an explicit --start-hash, skip
    // backward: the caller knows what they want.
    let mut aggregate = WalkStats::default();
    if args.walk_back_days > 0.0 && args.start_hash.is_none() {
        // 10 blocks/s × 86400 s/day = 864_000 DAA/day. Kaspa's BPS may
        // vary; the exact conversion isn't safety-critical — walk stops
        // when kaspad no longer serves a hash regardless of DAA.
        let max_daa_span = (args.walk_back_days * 864_000.0).ceil() as u64;
        info!(
            walk_back_days = args.walk_back_days,
            max_daa_span, "starting backward walk from pruning point"
        );
        let back = walk_backward_and_index(
            &client,
            pool.as_ref(),
            start_hash,
            max_daa_span,
            args.batch_size,
            args.dry_run,
        )
        .await?;
        info!(
            chain_blocks = back.chain_blocks,
            mergeset_blocks = back.mergeset_blocks,
            txs_seen = back.txs_seen,
            rows_inserted = back.rows_inserted,
            "Backward walk complete"
        );
        merge_stats(&mut aggregate, &back);
    }

    // Phase 2 — forward walk from pruning point (or the user-supplied
    // start_hash) to the sink, then optional --follow.
    let fwd = walk_and_index(
        &client,
        pool.as_ref(),
        start_hash,
        args.max_blocks,
        args.batch_size,
        args.dry_run,
        follow_interval,
    )
    .await?;
    merge_stats(&mut aggregate, &fwd);

    info!(
        chain_blocks = aggregate.chain_blocks,
        mergeset_blocks = aggregate.mergeset_blocks,
        txs_seen = aggregate.txs_seen,
        rows_inserted = aggregate.rows_inserted,
        "Backfill complete (backward + forward)"
    );
    Ok(())
}

fn merge_stats(into: &mut WalkStats, from: &WalkStats) {
    into.chain_blocks += from.chain_blocks;
    into.mergeset_blocks += from.mergeset_blocks;
    into.txs_seen += from.txs_seen;
    into.rows_inserted += from.rows_inserted;
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
    follow_interval: Option<Duration>,
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
            // Caught up to the sink. Flush any buffered rows, then either
            // exit (one-shot mode) or sleep and poll again (follow mode).
            flush(pool, dry_run, &mut pending, &mut stats).await?;
            match follow_interval {
                None => break,
                Some(dt) => {
                    info!(
                        cursor = %cursor,
                        sleep_secs = dt.as_secs(),
                        visited_dropped = visited.len(),
                        "caught up; sleeping before next follow poll"
                    );
                    // Bound memory over long follow runs. The dedup was
                    // only correct-critical within one page (chain block
                    // + mergeset overlap); across polls we'd only save
                    // wasted RPCs on rare mergeset re-appearance —
                    // negligible vs unbounded HashSet growth.
                    visited.clear();
                    tokio::time::sleep(dt).await;
                    continue;
                }
            }
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

/// Walk BACKWARD from `start_hash` via `verbose_data.selected_parent_hash`,
/// indexing each visited chain block + its mergeset. Stops when kaspad
/// stops serving a hash (natural retention-floor detection) or when we've
/// covered `max_daa_span` DAA units below the start.
///
/// Mergeset block fetch failures are non-fatal — the mergeset may span
/// slightly past the retention floor, and we don't want a stray missing
/// block to halt the whole walk. The chain-parent fetch failure IS
/// terminal (we can't walk further back without the block's own verbose
/// data).
async fn walk_backward_and_index(
    client: &kaspa_wrpc_client::KaspaRpcClient,
    pool: Option<&db::Pool>,
    start_hash: RpcHash,
    max_daa_span: u64,
    batch_size: usize,
    dry_run: bool,
) -> Result<WalkStats> {
    let mut stats = WalkStats::default();
    let mut cursor = start_hash;
    let mut visited: HashSet<[u8; 32]> = HashSet::new();
    let mut pending: Vec<TxIndexRow> = Vec::with_capacity(batch_size);
    let mut start_daa: Option<u64> = None;
    let mut last_progress_log = std::time::Instant::now();

    loop {
        // Fetch the current chain block with transactions AND verbose data.
        // `include_transactions=true` gives us both in one round-trip.
        let block = match client.get_block(cursor, true).await {
            Ok(b) => b,
            Err(e) => {
                info!(
                    hash = %cursor,
                    err = %e,
                    "backward walk stopping at retention floor (kaspad no longer serves this hash)"
                );
                break;
            }
        };

        let daa = block.header.daa_score;
        let start = *start_daa.get_or_insert(daa);
        let covered = start.saturating_sub(daa);
        if covered > max_daa_span {
            info!(
                covered_daa = covered,
                max_daa_span, "backward walk reached configured max DAA span"
            );
            break;
        }

        stats.chain_blocks += 1;

        // Chain block itself.
        if visited.insert(cursor.as_bytes()) {
            let daa_i64 = i64::try_from(daa).ok();
            let rows = tx_index::extract_rows(cursor.as_bytes(), daa_i64, &block.transactions);
            stats.txs_seen += rows.len() as u64;
            pending.extend(rows);
        }

        // Extract mergeset hashes + selected parent from verbose_data.
        let Some(verbose) = block.verbose_data else {
            warn!(
                hash = %cursor,
                "block missing verbose_data; cannot walk further back"
            );
            break;
        };

        // Mergeset — non-fatal on missing.
        for mh in verbose
            .merge_set_blues_hashes
            .iter()
            .chain(verbose.merge_set_reds_hashes.iter())
            .copied()
        {
            stats.mergeset_blocks += 1;
            if let Err(e) = index_block(client, mh, &mut visited, &mut pending, &mut stats).await
            {
                warn!(
                    hash = %mh,
                    err = %e,
                    "mergeset block unfetchable during backward walk (likely spans retention floor); skipping"
                );
            }
        }

        // Flush batches as they fill.
        if pending.len() >= batch_size {
            flush(pool, dry_run, &mut pending, &mut stats).await?;
        }

        if last_progress_log.elapsed() >= Duration::from_secs(20) {
            info!(
                chain_blocks = stats.chain_blocks,
                mergeset_blocks = stats.mergeset_blocks,
                txs_seen = stats.txs_seen,
                rows_inserted = stats.rows_inserted,
                current_daa = daa,
                covered_daa = covered,
                cursor = %cursor,
                "backward progress"
            );
            last_progress_log = std::time::Instant::now();
        }

        // Step: walk to the selected parent. All-zeros hash = genesis.
        if verbose.selected_parent_hash.as_bytes() == [0u8; 32] {
            info!("backward walk hit genesis (null selected_parent)");
            break;
        }
        cursor = verbose.selected_parent_hash;
    }

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
