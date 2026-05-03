//! `tidx backfill-kaspa` — populate `kaspa_l2_submissions` and `kaspa_entries`
//! by walking a Kaspa wRPC node's virtual chain.
//!
//! Use case: the realtime sync only sees Kaspa data within the production
//! kaspad's pruning window (~3 days). To recover the months of Igra L2
//! provenance that pre-date the tidx deployment, we run this CLI multiple
//! times against different wRPC sources covering different historical
//! windows (local kaspad-archive on each archive snapshot,
//! `wss://archival.kaspa.ws` for the recent ~30 days, etc).
//!
//! Idempotent: every insert uses `ON CONFLICT (kaspa_txid) DO NOTHING`, so
//! re-running the same window or rolling back/forward over overlapping
//! windows is safe. The realtime sync's data is preserved; this CLI only
//! adds rows that aren't already there.

use anyhow::{Context, Result, anyhow};
use clap::Args as ClapArgs;
use kaspa_rpc_core::{RpcHash, api::rpc::RpcApi};
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};

use tidx::config::Config;
use tidx::db;
use tidx::kaspa::client::connect_borsh_wrpc;
use tidx::kaspa::payload::{IgraKaspaPayload, IgraPayloadParser};

#[derive(ClapArgs)]
pub struct Args {
    /// Path to config file (used for DB connection only)
    #[arg(short, long, default_value = "config.toml")]
    pub config: PathBuf,

    /// Chain ID (uses first chain if not specified)
    #[arg(long)]
    pub chain_id: Option<u64>,

    /// Kaspa wRPC URL (Borsh) to walk for historical data. Examples:
    ///   - ws://127.0.0.1:17220 (local kaspad-archive)
    ///   - wss://archival.kaspa.ws (~30-day public archive)
    #[arg(long)]
    pub rpc: String,

    /// Optional explicit chain block hash to start walking from.
    /// Defaults to the wRPC node's current pruning_point.
    #[arg(long)]
    pub start_hash: Option<String>,

    /// Stop walking once we've processed this many accepted chain blocks
    /// (for debugging / chunked runs). Default: walk to current sink.
    #[arg(long)]
    pub max_blocks: Option<u64>,

    /// Don't write anything; only count what would be inserted.
    #[arg(long)]
    pub dry_run: bool,
}

/// One igra-relevant tx pulled out of a chain block during the walk.
/// Only the kaspa_txid and parsed payload survive the trip into the SQL
/// batch — accepted_block_hash / accepted_at are not stored on the final
/// tables (only on the pending tables, which we bypass for backfill).
#[derive(Debug, Clone)]
struct IgraTx {
    kaspa_txid: [u8; 32],
    payload: IgraKaspaPayload,
}

#[derive(Default, Debug)]
struct WalkStats {
    chain_blocks: u64,
    blocks_with_igra: u64,
    submissions_seen: u64,
    submissions_inserted: u64,
    entries_seen: u64,
    entries_inserted: u64,
}

pub async fn run(args: Args) -> Result<()> {
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

    let kaspa_cfg = chain
        .kaspa
        .as_ref()
        .ok_or_else(|| anyhow!("[kaspa] config block required for backfill-kaspa"))?;
    let parser = IgraPayloadParser::new(&kaspa_cfg.txid_prefix)?;

    // Pool is needed only for real (non-dry) writes. Dry-run can probe a wRPC
    // source without needing the prod DB to be reachable.
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
        utxo_indexed = server_info.has_utxo_index,
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

    let stats = walk_and_insert(
        &client,
        &parser,
        pool.as_ref(),
        start_hash,
        args.max_blocks,
        args.dry_run,
    )
    .await?;

    info!(
        chain_blocks = stats.chain_blocks,
        blocks_with_igra = stats.blocks_with_igra,
        submissions_seen = stats.submissions_seen,
        submissions_inserted = stats.submissions_inserted,
        entries_seen = stats.entries_seen,
        entries_inserted = stats.entries_inserted,
        "Backfill complete"
    );
    Ok(())
}

/// Walk the virtual chain forward from `start_hash`, fetching block bodies for
/// any chain block that contains at least one `97b1`-prefix txid, parsing the
/// igra payload, and inserting into the final tables (idempotent).
async fn walk_and_insert(
    client: &kaspa_wrpc_client::KaspaRpcClient,
    parser: &IgraPayloadParser,
    pool: Option<&db::Pool>,
    start_hash: RpcHash,
    max_blocks: Option<u64>,
    dry_run: bool,
) -> Result<WalkStats> {
    let mut stats = WalkStats::default();
    let mut cursor = start_hash;
    let mut last_progress_log = std::time::Instant::now();

    loop {
        // Page through the virtual chain. include_accepted_transaction_ids = true
        // gives us per-chain-block lists of txids accepted at that point.
        let resp = client
            .get_virtual_chain_from_block(cursor, true, None)
            .await
            .with_context(|| {
                format!("get_virtual_chain_from_block from {cursor}")
            })?;

        // Empty page means we caught up to the sink.
        if resp.added_chain_block_hashes.is_empty() {
            break;
        }

        // Build a hash → accepted txid set map for the page.
        // accepted_transaction_ids is in the same order as added_chain_block_hashes.
        let txid_sets: std::collections::HashMap<RpcHash, Vec<RpcHash>> = resp
            .accepted_transaction_ids
            .into_iter()
            .map(|a| (a.accepting_block_hash, a.accepted_transaction_ids))
            .collect();

        for chain_block_hash in &resp.added_chain_block_hashes {
            stats.chain_blocks += 1;

            // Only fetch the block body if at least one accepted txid in this
            // chain block matches the igra prefix. Most chain blocks have no
            // igra txs, so this short-circuit is the bulk of the speedup.
            let accepted_ids = txid_sets
                .get(chain_block_hash)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let has_igra_candidate = accepted_ids
                .iter()
                .any(|id| parser.txid_matches(&id.as_bytes()));
            if !has_igra_candidate {
                cursor = *chain_block_hash;
                if let Some(cap) = max_blocks {
                    if stats.chain_blocks >= cap {
                        return Ok(stats);
                    }
                }
                continue;
            }
            stats.blocks_with_igra += 1;

            let chain_block = client
                .get_block(*chain_block_hash, true)
                .await
                .with_context(|| format!("get_block {chain_block_hash}"))?;

            // Track only the `97b1`-prefix txids accepted at this chain block.
            // We drain them as we find them — first from the chain block's own
            // body, then (if any are still missing) from each mergeset block.
            // In Kaspa BlockDAG, most user txs live in mergeset blocks rather
            // than the chain block itself, so the mergeset pass is the bulk
            // of the matches.
            let mut remaining: std::collections::HashSet<[u8; 32]> = accepted_ids
                .iter()
                .map(|h| h.as_bytes())
                .filter(|t| parser.txid_matches(t))
                .collect();
            let mut igra_txs: Vec<IgraTx> = Vec::new();
            drain_block_txs(&chain_block.transactions, &mut remaining, parser, &mut igra_txs);

            if !remaining.is_empty() {
                if let Some(verbose) = &chain_block.verbose_data {
                    let mergeset: Vec<RpcHash> = verbose
                        .merge_set_blues_hashes
                        .iter()
                        .chain(verbose.merge_set_reds_hashes.iter())
                        .copied()
                        .collect();
                    for mh in mergeset {
                        if remaining.is_empty() {
                            break;
                        }
                        let merged = client
                            .get_block(mh, true)
                            .await
                            .with_context(|| format!("get_block (mergeset) {mh}"))?;
                        drain_block_txs(&merged.transactions, &mut remaining, parser, &mut igra_txs);
                    }
                }
                if !remaining.is_empty() {
                    warn!(
                        chain_block = %chain_block_hash,
                        missing = remaining.len(),
                        "accepted Igra txids not found in chain block or its mergeset"
                    );
                }
            }

            // Tally + write.
            for itx in &igra_txs {
                match itx.payload {
                    IgraKaspaPayload::L2Submission { .. } => stats.submissions_seen += 1,
                    IgraKaspaPayload::Entry { .. } => stats.entries_seen += 1,
                }
            }
            if !dry_run && !igra_txs.is_empty() {
                if let Some(pool) = pool {
                    let (subs, ents) = insert_batch(pool, &igra_txs).await?;
                    stats.submissions_inserted += subs;
                    stats.entries_inserted += ents;
                }
            }

            cursor = *chain_block_hash;
            if last_progress_log.elapsed() >= Duration::from_secs(20) {
                info!(
                    chain_blocks = stats.chain_blocks,
                    blocks_with_igra = stats.blocks_with_igra,
                    submissions_seen = stats.submissions_seen,
                    submissions_inserted = stats.submissions_inserted,
                    entries_seen = stats.entries_seen,
                    entries_inserted = stats.entries_inserted,
                    cursor = %chain_block_hash,
                    "progress"
                );
                last_progress_log = std::time::Instant::now();
            }
            if let Some(cap) = max_blocks {
                if stats.chain_blocks >= cap {
                    return Ok(stats);
                }
            }
        }
    }

    Ok(stats)
}

/// Drain igra-relevant txs from `block_txs` whose txid is in `remaining`.
/// Removes hits from `remaining` and appends parsed payloads to `out`.
/// Only `97b1`-prefix txs are checked; everything else is skipped.
fn drain_block_txs(
    block_txs: &[kaspa_rpc_core::RpcTransaction],
    remaining: &mut std::collections::HashSet<[u8; 32]>,
    parser: &IgraPayloadParser,
    out: &mut Vec<IgraTx>,
) {
    for tx in block_txs {
        let Some(verbose) = &tx.verbose_data else {
            continue;
        };
        let txid: [u8; 32] = verbose.transaction_id.as_bytes();
        if !remaining.remove(&txid) {
            continue;
        }
        let parsed = match parser.parse(&txid, &tx.payload) {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    kaspa_txid = %hex::encode(txid),
                    err = %e,
                    "skipping malformed igra payload"
                );
                continue;
            }
        };
        if let Some(payload) = parsed {
            out.push(IgraTx {
                kaspa_txid: txid,
                payload,
            });
        }
    }
}

/// Insert a batch of igra txs into the final tables. Idempotent via
/// `ON CONFLICT DO NOTHING` on the `kaspa_txid` PK.
async fn insert_batch(pool: &db::Pool, txs: &[IgraTx]) -> Result<(u64, u64)> {
    let mut client = pool.get().await?;
    let tx = client.transaction().await?;

    let mut sub_l2_hashes: Vec<&[u8]> = Vec::new();
    let mut sub_kaspa_txids: Vec<&[u8]> = Vec::new();
    let mut entry_kaspa_txids: Vec<&[u8]> = Vec::new();
    let mut entry_recipients: Vec<&[u8]> = Vec::new();
    let mut entry_amounts: Vec<i64> = Vec::new();

    for itx in txs {
        match &itx.payload {
            IgraKaspaPayload::L2Submission { l2_tx_hash } => {
                sub_l2_hashes.push(l2_tx_hash);
                sub_kaspa_txids.push(&itx.kaspa_txid);
            }
            IgraKaspaPayload::Entry {
                recipient,
                amount_sompi,
            } => {
                entry_kaspa_txids.push(&itx.kaspa_txid);
                entry_recipients.push(recipient);
                entry_amounts.push(i64::try_from(*amount_sompi)
                    .map_err(|_| anyhow!("amount_sompi overflow on tx {}", hex::encode(itx.kaspa_txid)))?);
            }
        }
    }

    let mut subs_inserted = 0u64;
    if !sub_l2_hashes.is_empty() {
        let n = tx
            .execute(
                "INSERT INTO kaspa_l2_submissions (l2_tx_hash, kaspa_txid)
                 SELECT * FROM UNNEST($1::bytea[], $2::bytea[])
                 ON CONFLICT (l2_tx_hash) DO NOTHING",
                &[&sub_l2_hashes, &sub_kaspa_txids],
            )
            .await
            .context("INSERT kaspa_l2_submissions batch")?;
        subs_inserted = n;
    }

    let mut entries_inserted = 0u64;
    if !entry_kaspa_txids.is_empty() {
        let n = tx
            .execute(
                "INSERT INTO kaspa_entries (kaspa_txid, recipient, amount_sompi)
                 SELECT * FROM UNNEST($1::bytea[], $2::bytea[], $3::int8[])
                 ON CONFLICT (kaspa_txid) DO NOTHING",
                &[&entry_kaspa_txids, &entry_recipients, &entry_amounts],
            )
            .await
            .context("INSERT kaspa_entries batch")?;
        entries_inserted = n;
    }

    tx.commit().await?;
    Ok((subs_inserted, entries_inserted))
}
