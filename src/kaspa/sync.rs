use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use kaspa_rpc_core::{RpcDataVerbosityLevel, api::rpc::RpcApi};
use kaspa_wrpc_client::KaspaRpcClient;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::config::{ChainConfig, KaspaConfig};
use crate::kaspa::clickhouse::KaspaClickHouseMirror;
use crate::kaspa::client::connect_borsh_wrpc;
use crate::kaspa::payload::{IgraKaspaPayload, IgraPayloadParser};
use crate::kaspa::writer::{KaspaProvenanceWriter, PendingEntry, PendingL2Submission};

/// Unified view of a single virtual-chain progression round. Both the v2 server
/// path and the v1 fallback produce this shape so downstream code stays protocol-agnostic.
struct ChainUpdate {
    removed: Vec<[u8; 32]>,
    added: Vec<AddedChainBlock>,
    last_daa_score: Option<u64>,
}

struct AddedChainBlock {
    hash: [u8; 32],
    accepted_at: DateTime<Utc>,
    accepted_transactions: Vec<AcceptedTx>,
}

struct AcceptedTx {
    txid: [u8; 32],
    payload: Vec<u8>,
}

pub async fn run_kaspa_provenance_sync(
    chain: ChainConfig,
    pool: crate::db::Pool,
    clickhouse: Option<KaspaClickHouseMirror>,
    mut shutdown_rx: broadcast::Receiver<()>,
) {
    let Some(kaspa) = chain.kaspa.clone().filter(|cfg| cfg.enabled) else {
        return;
    };

    loop {
        if let Err(e) = run_once(
            chain.clone(),
            pool.clone(),
            kaspa.clone(),
            clickhouse.clone(),
            shutdown_rx.resubscribe(),
        )
        .await
        {
            error!(chain_id = chain.chain_id, error = %e, "Kaspa provenance sync failed; retrying");
            if let Ok(writer) = KaspaProvenanceWriter::new(pool.clone(), kaspa.promotion_delay_secs)
            {
                let _ = writer.record_error(&e.to_string()).await;
            }
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    info!(chain_id = chain.chain_id, "Kaspa provenance sync shutting down");
                    return;
                }
                _ = tokio::time::sleep(Duration::from_secs(10)) => {}
            }
        } else {
            return;
        }
    }
}

async fn run_once(
    chain: ChainConfig,
    pool: crate::db::Pool,
    kaspa: KaspaConfig,
    clickhouse: Option<KaspaClickHouseMirror>,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<()> {
    let parser = Arc::new(IgraPayloadParser::new(&kaspa.txid_prefix)?);
    let writer = KaspaProvenanceWriter::new(pool, kaspa.promotion_delay_secs)?;
    writer
        .ensure_meta(chain.chain_id, &kaspa, parser.txid_prefix())
        .await?;

    if let Some(clickhouse) = &clickhouse {
        clickhouse.ensure_schema().await?;
    }

    let client = connect_borsh_wrpc(&kaspa.rpc_url).await?;
    let server_info = client.get_server_info().await?;
    let use_v2 = supports_v2(&server_info.server_version);
    info!(
        chain_id = chain.chain_id,
        kaspa_rpc = %kaspa.rpc_url,
        kaspa_version = %server_info.server_version,
        kaspa_network = %server_info.network_id,
        txid_prefix = %parser.txid_prefix_hex(),
        rpc_method = if use_v2 {
            "get_virtual_chain_from_block_v2"
        } else {
            "get_virtual_chain_from_block (v1 fallback)"
        },
        "Kaspa provenance sync connected"
    );

    let dag_info = client.get_block_dag_info().await?;
    let state = writer.load_state(kaspa.initial_tip_distance).await?;
    let mut checkpoint = state
        .checkpoint_hash
        .unwrap_or_else(|| dag_info.pruning_point_hash.as_bytes());
    let mut tip_distance = state.tip_distance.max(1);
    let poll_interval = Duration::from_millis(kaspa.poll_interval_ms.max(100));

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                let _ = client.disconnect().await;
                return Ok(());
            }
            _ = tokio::time::sleep(poll_interval) => {}
        }

        let update = if use_v2 {
            fetch_chain_update_v2(&client, &checkpoint, tip_distance, parser.as_ref()).await?
        } else {
            fetch_chain_update_v1(&client, &checkpoint, parser.as_ref()).await?
        };

        if update.removed.is_empty() && update.added.is_empty() {
            mirror_promotions(&writer, clickhouse.as_ref()).await?;
            continue;
        }

        let ChainUpdate {
            removed,
            added,
            last_daa_score,
        } = update;

        let deleted = writer.delete_pending_for_removed_blocks(&removed).await?;
        if deleted > 0 {
            tip_distance = tip_distance.saturating_add(1);
            warn!(
                chain_id = chain.chain_id,
                deleted,
                tip_distance,
                "Kaspa provenance removed pending rows after virtual-chain reorg"
            );
        }

        let (pending_l2, pending_entries) = extract_pending_rows(&added, parser.as_ref())?;
        writer.insert_pending(&pending_l2, &pending_entries).await?;
        mirror_promotions(&writer, clickhouse.as_ref()).await?;

        if let Some(last) = added.last() {
            checkpoint = last.hash;
            writer
                .update_success(
                    &checkpoint,
                    &dag_info.sink.as_bytes(),
                    last_daa_score,
                    tip_distance,
                )
                .await?;
        }

        debug!(
            chain_id = chain.chain_id,
            pending_l2 = pending_l2.len(),
            pending_entries = pending_entries.len(),
            added = added.len(),
            removed = removed.len(),
            "Kaspa provenance batch processed"
        );
    }
}

fn supports_v2(server_version: &str) -> bool {
    match parse_kaspa_version(server_version) {
        Some((major, minor, _)) => (major, minor) >= (1, 1),
        None => true,
    }
}

fn parse_kaspa_version(s: &str) -> Option<(u16, u16, u16)> {
    let core = s.split('-').next().unwrap_or(s).trim();
    let mut parts = core.split('.');
    let major: u16 = parts.next()?.parse().ok()?;
    let minor: u16 = parts.next()?.parse().ok()?;
    let patch: u16 = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Group a v1 response's accepted transaction IDs by their accepting chain block hash,
/// keeping only those whose Kaspa txid begins with the configured Igra prefix. Chain
/// blocks with no prefix-matching accepted txs are absent from the result entirely so
/// callers can skip the per-block `get_block` round-trip.
///
/// **Kept only for tests and historical reference.** Live sync no longer uses this —
/// the accepted-txid filter was too restrictive (missed ~20% of 97b1-prefix L1 txs
/// that live in mergeset-red blocks; see [`collect_prefix_txs_from_slice`]).
#[cfg_attr(not(test), allow(dead_code))]
fn group_prefixed_accepted_txids(
    response: &kaspa_rpc_core::GetVirtualChainFromBlockResponse,
    parser: &IgraPayloadParser,
) -> HashMap<[u8; 32], Vec<[u8; 32]>> {
    let mut out: HashMap<[u8; 32], Vec<[u8; 32]>> = HashMap::new();
    for entry in &response.accepted_transaction_ids {
        let block_hash = entry.accepting_block_hash.as_bytes();
        let filtered: Vec<[u8; 32]> = entry
            .accepted_transaction_ids
            .iter()
            .map(|t| t.as_bytes())
            .filter(|id| parser.txid_matches(id))
            .collect();
        if !filtered.is_empty() {
            out.insert(block_hash, filtered);
        }
    }
    out
}

/// Scan a slice of `RpcTransaction` and append every one whose txid starts with
/// the Igra prefix to `out`. Uses `visited` to dedup within a single chain-block
/// unit (chain block + all its mergeset blocks) — a tx included both in the
/// chain block body and in a mergeset gets appended only once.
///
/// Why this replaces `drain_matching_txs`: the older code filtered by whether
/// the txid was in kaspad's `accepted_transaction_ids` list. But that list only
/// includes txs the chain block VOTED FOR (blue mergeset + own body). Igra L2
/// processes EVERY 97b1-prefix Kaspa tx in the DAG, including ones in
/// mergeset-red blocks that never got chain-accepted. Filtering by
/// accepted_transaction_ids drops ~20% of production 97b1 traffic — measured
/// against tidx.igralabs.com on 2026-08-04: 26,591 of 134,614 recent
/// 97b1-prefix txs never made it into `kaspa_l2_submissions`. The txs are
/// on-chain, the parser accepts them (0x4/0x5), and the L2 tx hashes they
/// carry exist in `txs` — they just weren't in `accepted_transaction_ids`.
fn collect_prefix_txs_from_slice(
    txs: &[kaspa_rpc_core::RpcTransaction],
    parser: &IgraPayloadParser,
    visited: &mut HashSet<[u8; 32]>,
    out: &mut Vec<AcceptedTx>,
) {
    for tx in txs {
        let Some(verbose) = &tx.verbose_data else {
            continue;
        };
        let txid: [u8; 32] = verbose.transaction_id.as_bytes();
        if !parser.txid_matches(&txid) {
            continue;
        }
        if !visited.insert(txid) {
            continue;
        }
        out.push(AcceptedTx {
            txid,
            payload: tx.payload.clone(),
        });
    }
}

/// Fetch a chain block and every block in its mergeset (blues + reds), then
/// collect every 97b1-prefix tx from any of them. Returns `(accepted_at,
/// daa_score, txs)`.
///
/// This is the sole "which Kaspa L1 txs did this chain block introduce to the
/// Igra L2's view" primitive. Both the v1 and v2 fetch paths call it for each
/// added chain block.
async fn expand_and_collect_igra_txs(
    client: &KaspaRpcClient,
    chain_block_hash: kaspa_rpc_core::RpcHash,
    parser: &IgraPayloadParser,
) -> Result<(DateTime<Utc>, u64, Vec<AcceptedTx>)> {
    let chain_block = client
        .get_block(chain_block_hash, true)
        .await
        .with_context(|| {
            format!(
                "get_block failed for chain block {}",
                chain_block_hash
            )
        })?;

    let accepted_at = i64::try_from(chain_block.header.timestamp)
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .unwrap_or_else(Utc::now);
    let daa_score = chain_block.header.daa_score;

    let mut visited: HashSet<[u8; 32]> = HashSet::new();
    let mut out: Vec<AcceptedTx> = Vec::new();
    collect_prefix_txs_from_slice(&chain_block.transactions, parser, &mut visited, &mut out);

    if let Some(verbose) = &chain_block.verbose_data {
        let mergeset_hashes: Vec<_> = verbose
            .merge_set_blues_hashes
            .iter()
            .chain(verbose.merge_set_reds_hashes.iter())
            .copied()
            .collect();
        for mh in mergeset_hashes {
            let merged = client.get_block(mh, true).await.with_context(|| {
                format!(
                    "get_block failed for mergeset block {}",
                    hex::encode(mh.as_bytes())
                )
            })?;
            collect_prefix_txs_from_slice(&merged.transactions, parser, &mut visited, &mut out);
        }
    }

    Ok((accepted_at, daa_score, out))
}

/// Drain `wanted` of any txids found in the provided iterator, appending matching
/// (txid, payload) pairs to `out`. Items whose txid is `None` (e.g. RpcTransaction
/// without verbose_data) are skipped; the iterator stops early once `wanted` empties.
///
/// **Kept only for tests and historical reference.** Live sync no longer uses this —
/// see [`collect_prefix_txs_from_slice`] for why.
#[cfg_attr(not(test), allow(dead_code))]
fn drain_matching_txs<'a, I>(
    txs: I,
    wanted: &mut HashSet<[u8; 32]>,
    out: &mut Vec<AcceptedTx>,
) where
    I: IntoIterator<Item = (Option<[u8; 32]>, &'a [u8])>,
{
    for (txid, payload) in txs {
        if wanted.is_empty() {
            break;
        }
        let Some(txid) = txid else {
            continue;
        };
        if wanted.remove(&txid) {
            out.push(AcceptedTx {
                txid,
                payload: payload.to_vec(),
            });
        }
    }
}


async fn fetch_chain_update_v2(
    client: &KaspaRpcClient,
    checkpoint: &[u8; 32],
    tip_distance: u64,
    parser: &IgraPayloadParser,
) -> Result<ChainUpdate> {
    let response = client
        .get_virtual_chain_from_block_v2(
            (*checkpoint).into(),
            Some(RpcDataVerbosityLevel::Full),
            Some(tip_distance),
        )
        .await
        .with_context(|| {
            format!(
                "get_virtual_chain_from_block_v2 failed from {}",
                hex::encode(checkpoint)
            )
        })?;

    let removed: Vec<[u8; 32]> = response
        .removed_chain_block_hashes
        .iter()
        .map(|h| h.as_bytes())
        .collect();

    // We can't trust `group.accepted_transactions` alone — v2's response only
    // contains chain-accepted txs, which excludes ~20% of 97b1-prefix L1 txs
    // that live in mergeset-red blocks (see `collect_prefix_txs_from_slice`).
    // For each chain block, do the full chain-body + mergeset walk via
    // `expand_and_collect_igra_txs`. We DO use the v2 header data (hash, daa)
    // as-is — that's free and saves one extra `get_block` when it's provided.
    let mut added: Vec<AddedChainBlock> = Vec::with_capacity(
        response.chain_block_accepted_transactions.len(),
    );
    let mut last_daa_score: Option<u64> = None;
    for group in response.chain_block_accepted_transactions.iter() {
        let hash_rpc = group
            .chain_block_header
            .hash
            .ok_or_else(|| anyhow!("V2 response missing chain block hash"))?;
        let hash = hash_rpc.as_bytes();

        let (accepted_at, daa_score, accepted_transactions) =
            expand_and_collect_igra_txs(client, hash_rpc, parser).await?;
        last_daa_score = Some(daa_score);

        added.push(AddedChainBlock {
            hash,
            accepted_at,
            accepted_transactions,
        });
    }

    Ok(ChainUpdate {
        removed,
        added,
        last_daa_score,
    })
}

async fn fetch_chain_update_v1(
    client: &KaspaRpcClient,
    checkpoint: &[u8; 32],
    parser: &IgraPayloadParser,
) -> Result<ChainUpdate> {
    let response = client
        .get_virtual_chain_from_block((*checkpoint).into(), true, None)
        .await
        .with_context(|| {
            format!(
                "get_virtual_chain_from_block (v1) failed from {}",
                hex::encode(checkpoint)
            )
        })?;

    let removed: Vec<[u8; 32]> = response
        .removed_chain_block_hashes
        .iter()
        .map(|h| h.as_bytes())
        .collect();

    let added_hashes: Vec<[u8; 32]> = response
        .added_chain_block_hashes
        .iter()
        .map(|h| h.as_bytes())
        .collect();

    let last_hash = added_hashes.last().copied();
    let mut added: Vec<AddedChainBlock> = Vec::with_capacity(added_hashes.len());
    let mut last_daa_score: Option<u64> = None;

    for block_hash in &added_hashes {
        let is_last = Some(*block_hash) == last_hash;

        // Fetch chain block + mergeset, collect ALL 97b1-prefix txs.
        // No pre-filter by `accepted_transaction_ids`: Igra L2 processes every
        // 97b1 Kaspa tx it sees, including mergeset-reds. See
        // `collect_prefix_txs_from_slice` docstring for the diagnosis.
        let (accepted_at, daa_score, accepted_transactions) =
            expand_and_collect_igra_txs(client, (*block_hash).into(), parser).await?;

        if is_last {
            last_daa_score = Some(daa_score);
        }

        added.push(AddedChainBlock {
            hash: *block_hash,
            accepted_at,
            accepted_transactions,
        });
    }

    Ok(ChainUpdate {
        removed,
        added,
        last_daa_score,
    })
}

fn extract_pending_rows(
    added: &[AddedChainBlock],
    parser: &IgraPayloadParser,
) -> Result<(Vec<PendingL2Submission>, Vec<PendingEntry>)> {
    let mut l2_submissions = Vec::new();
    let mut entries = Vec::new();

    for group in added {
        for tx in &group.accepted_transactions {
            let parsed = match parser.parse(&tx.txid, &tx.payload) {
                Ok(parsed) => parsed,
                Err(error) => {
                    debug!(
                        kaspa_txid = %hex::encode(tx.txid),
                        %error,
                        "Skipping malformed Igra Kaspa payload"
                    );
                    continue;
                }
            };

            match parsed {
                Some(IgraKaspaPayload::L2Submission { l2_tx_hash }) => {
                    l2_submissions.push(PendingL2Submission {
                        l2_tx_hash,
                        kaspa_txid: tx.txid,
                        accepted_chain_block_hash: group.hash,
                        accepted_at: group.accepted_at,
                    });
                }
                Some(IgraKaspaPayload::Entry {
                    recipient,
                    amount_sompi,
                }) => {
                    entries.push(PendingEntry {
                        kaspa_txid: tx.txid,
                        recipient,
                        amount_sompi,
                        accepted_chain_block_hash: group.hash,
                        accepted_at: group.accepted_at,
                    });
                }
                None => {}
            }
        }
    }

    Ok((l2_submissions, entries))
}

async fn mirror_promotions(
    writer: &KaspaProvenanceWriter,
    clickhouse: Option<&KaspaClickHouseMirror>,
) -> Result<()> {
    let promoted = writer.promote_due().await?;
    if let Some(clickhouse) = clickhouse {
        clickhouse
            .write_l2_submissions(&promoted.l2_submissions)
            .await?;
        clickhouse.write_entries(&promoted.entries).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_rpc_core::{
        GetVirtualChainFromBlockResponse, RpcAcceptedTransactionIds, RpcHash,
    };

    // ---------- version parsing & dispatch ----------

    #[test]
    fn parses_version_strings() {
        assert_eq!(parse_kaspa_version("1.0.1"), Some((1, 0, 1)));
        assert_eq!(parse_kaspa_version("1.1.0"), Some((1, 1, 0)));
        assert_eq!(parse_kaspa_version("1.1.0-rc.2"), Some((1, 1, 0)));
        assert_eq!(parse_kaspa_version("0.15.4"), Some((0, 15, 4)));
        assert_eq!(parse_kaspa_version(" 1.2.3 "), Some((1, 2, 3))); // trims
        assert_eq!(parse_kaspa_version(""), None);
        assert_eq!(parse_kaspa_version("garbage"), None);
        assert_eq!(parse_kaspa_version("1.0"), None); // patch missing
        assert_eq!(parse_kaspa_version("a.b.c"), None);
    }

    #[test]
    fn v2_gated_on_minor_version() {
        assert!(!supports_v2("1.0.0"));
        assert!(!supports_v2("1.0.1"));
        assert!(!supports_v2("0.99.99"));
        assert!(supports_v2("1.1.0"));
        assert!(supports_v2("1.1.0-rc.2"));
        assert!(supports_v2("1.2.5"));
        assert!(supports_v2("2.0.0"));
        assert!(supports_v2("unknown")); // unparseable defaults to v2
    }

    // ---------- group_prefixed_accepted_txids ----------

    /// Helper: construct a 32-byte hash from a single byte (e.g., `h(7)` = [7; 32]).
    fn h(b: u8) -> RpcHash {
        [b; 32].into()
    }

    /// Helper: a 32-byte txid whose first 2 bytes are `0x97 0xb1` (the Igra mainnet
    /// prefix), with `tag` filling the rest. Matches `IgraPayloadParser::new("97b1")`.
    fn igra_txid(tag: u8) -> RpcHash {
        let mut bytes = [tag; 32];
        bytes[0] = 0x97;
        bytes[1] = 0xb1;
        bytes.into()
    }

    /// Helper: a 32-byte txid that does NOT start with `0x97 0xb1`.
    fn other_txid(tag: u8) -> RpcHash {
        [tag; 32].into() // first byte = tag != 0x97
    }

    fn parser() -> IgraPayloadParser {
        IgraPayloadParser::new("97b1").unwrap()
    }

    fn v1_response(
        accepted: Vec<RpcAcceptedTransactionIds>,
    ) -> GetVirtualChainFromBlockResponse {
        GetVirtualChainFromBlockResponse {
            removed_chain_block_hashes: vec![],
            added_chain_block_hashes: vec![],
            accepted_transaction_ids: accepted,
        }
    }

    #[test]
    fn group_prefixed_returns_empty_for_empty_response() {
        let result = group_prefixed_accepted_txids(&v1_response(vec![]), &parser());
        assert!(result.is_empty());
    }

    #[test]
    fn group_prefixed_keeps_only_igra_prefixed_txids() {
        let block_a = h(0xAA);
        let response = v1_response(vec![RpcAcceptedTransactionIds {
            accepting_block_hash: block_a,
            accepted_transaction_ids: vec![
                igra_txid(0x01),  // matches
                other_txid(0x42), // does NOT match (starts with 0x42)
                igra_txid(0x02),  // matches
                other_txid(0x55), // does NOT match
            ],
        }]);

        let result = group_prefixed_accepted_txids(&response, &parser());

        assert_eq!(result.len(), 1);
        let entries = result.get(&block_a.as_bytes()).expect("block_a present");
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&igra_txid(0x01).as_bytes()));
        assert!(entries.contains(&igra_txid(0x02).as_bytes()));
    }

    #[test]
    fn group_prefixed_omits_blocks_with_no_matching_txids() {
        let block_a = h(0xAA);
        let block_b = h(0xBB);
        let response = v1_response(vec![
            RpcAcceptedTransactionIds {
                accepting_block_hash: block_a,
                accepted_transaction_ids: vec![igra_txid(0x10)],
            },
            RpcAcceptedTransactionIds {
                accepting_block_hash: block_b,
                // none match the 0x97 0xb1 prefix
                accepted_transaction_ids: vec![other_txid(0x33), other_txid(0x44)],
            },
        ]);

        let result = group_prefixed_accepted_txids(&response, &parser());

        assert_eq!(result.len(), 1, "block_b should be absent");
        assert!(result.contains_key(&block_a.as_bytes()));
        assert!(!result.contains_key(&block_b.as_bytes()));
    }

    #[test]
    fn group_prefixed_handles_multiple_blocks() {
        let block_a = h(0xAA);
        let block_b = h(0xBB);
        let response = v1_response(vec![
            RpcAcceptedTransactionIds {
                accepting_block_hash: block_a,
                accepted_transaction_ids: vec![igra_txid(0x01)],
            },
            RpcAcceptedTransactionIds {
                accepting_block_hash: block_b,
                accepted_transaction_ids: vec![igra_txid(0x02), igra_txid(0x03)],
            },
        ]);

        let result = group_prefixed_accepted_txids(&response, &parser());

        assert_eq!(result.len(), 2);
        assert_eq!(result[&block_a.as_bytes()].len(), 1);
        assert_eq!(result[&block_b.as_bytes()].len(), 2);
    }

    // ---------- drain_matching_txs ----------

    fn txid(b: u8) -> [u8; 32] {
        [b; 32]
    }

    #[test]
    fn drain_collects_matching_and_drains_wanted() {
        let mut wanted: HashSet<[u8; 32]> = [txid(1), txid(2)].into_iter().collect();
        let mut out: Vec<AcceptedTx> = Vec::new();

        let txs: Vec<(Option<[u8; 32]>, &[u8])> = vec![
            (Some(txid(1)), b"payload-1".as_slice()),
            (Some(txid(2)), b"payload-2".as_slice()),
        ];

        drain_matching_txs(txs, &mut wanted, &mut out);

        assert!(wanted.is_empty(), "all wanted txids should be removed");
        assert_eq!(out.len(), 2);
        let payloads: Vec<&[u8]> = out.iter().map(|t| t.payload.as_slice()).collect();
        assert!(payloads.contains(&b"payload-1".as_slice()));
        assert!(payloads.contains(&b"payload-2".as_slice()));
    }

    #[test]
    fn drain_leaves_unwanted_txs_alone() {
        let mut wanted: HashSet<[u8; 32]> = [txid(1)].into_iter().collect();
        let mut out: Vec<AcceptedTx> = Vec::new();

        let txs: Vec<(Option<[u8; 32]>, &[u8])> = vec![
            (Some(txid(99)), b"unrelated".as_slice()),
            (Some(txid(1)), b"hit".as_slice()),
            (Some(txid(98)), b"unrelated2".as_slice()),
        ];

        drain_matching_txs(txs, &mut wanted, &mut out);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].txid, txid(1));
        assert_eq!(&out[0].payload, b"hit");
        assert!(wanted.is_empty());
    }

    #[test]
    fn drain_keeps_remaining_when_not_all_present() {
        let mut wanted: HashSet<[u8; 32]> = [txid(1), txid(2), txid(3)].into_iter().collect();
        let mut out: Vec<AcceptedTx> = Vec::new();

        let txs: Vec<(Option<[u8; 32]>, &[u8])> =
            vec![(Some(txid(2)), b"only-2".as_slice())];

        drain_matching_txs(txs, &mut wanted, &mut out);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].txid, txid(2));
        assert_eq!(wanted.len(), 2);
        assert!(wanted.contains(&txid(1)));
        assert!(wanted.contains(&txid(3)));
        assert!(!wanted.contains(&txid(2)));
    }

    #[test]
    fn drain_skips_txs_with_no_txid() {
        let mut wanted: HashSet<[u8; 32]> = [txid(1)].into_iter().collect();
        let mut out: Vec<AcceptedTx> = Vec::new();

        let txs: Vec<(Option<[u8; 32]>, &[u8])> = vec![
            (None, b"no-verbose-data".as_slice()), // simulates RpcTransaction with verbose_data == None
            (Some(txid(1)), b"hit".as_slice()),
        ];

        drain_matching_txs(txs, &mut wanted, &mut out);

        assert_eq!(out.len(), 1, "the None entry must not appear in output");
        assert_eq!(out[0].txid, txid(1));
        assert!(wanted.is_empty());
    }

    #[test]
    fn drain_only_collects_each_txid_once() {
        // Even if the same txid appears twice (which would be a kaspad bug, but the
        // contract is "remove() based"), we should only collect it once.
        let mut wanted: HashSet<[u8; 32]> = [txid(1)].into_iter().collect();
        let mut out: Vec<AcceptedTx> = Vec::new();

        let txs: Vec<(Option<[u8; 32]>, &[u8])> = vec![
            (Some(txid(1)), b"first".as_slice()),
            (Some(txid(1)), b"second-duplicate".as_slice()),
        ];

        drain_matching_txs(txs, &mut wanted, &mut out);

        assert_eq!(out.len(), 1);
        assert_eq!(&out[0].payload, b"first");
    }

    #[test]
    fn drain_short_circuits_when_wanted_empty() {
        // After all wanted txids are matched, the iterator should not be exhausted —
        // important for performance when scanning blocks with thousands of txs.
        let mut wanted: HashSet<[u8; 32]> = [txid(1)].into_iter().collect();
        let mut out: Vec<AcceptedTx> = Vec::new();

        // Use a flag to verify we stopped iterating.
        let observed_count = std::cell::Cell::new(0);
        let txs = (0u8..=255).map(|i| {
            observed_count.set(observed_count.get() + 1);
            (Some(txid(i)), b"x".as_slice())
        });

        drain_matching_txs(txs, &mut wanted, &mut out);

        assert_eq!(out.len(), 1);
        assert!(
            observed_count.get() < 256,
            "should have stopped early; observed {} of 256",
            observed_count.get()
        );
    }

    // ---------- collect_prefix_txs_from_slice ----------
    // These tests document the invariant that ALL 97b1-prefix txs get collected,
    // not just the ones the chain block voted for. Regressions on this filter
    // would silently drop L1→L2 links (see 2026-08-04 diagnosis).

    use kaspa_rpc_core::{
        RpcSubnetworkId, RpcTransactionInput, RpcTransactionOutpoint, RpcTransactionOutput,
        RpcTransactionVerboseData,
    };

    fn make_tx(txid_first_bytes: &[u8], payload: Vec<u8>) -> kaspa_rpc_core::RpcTransaction {
        let empty_spk: kaspa_rpc_core::RpcScriptPublicKey =
            serde_json::from_str(r#"{"version":0,"script":""}"#).unwrap();
        let mut id = [0u8; 32];
        id[..txid_first_bytes.len()].copy_from_slice(txid_first_bytes);
        // Extra padding after the prefix to make each txid unique when tests
        // pass the same prefix — the last byte varies with the length of prefix.
        id[31] = txid_first_bytes.len() as u8;
        kaspa_rpc_core::RpcTransaction {
            version: 0,
            inputs: vec![RpcTransactionInput {
                previous_outpoint: RpcTransactionOutpoint {
                    transaction_id: [0u8; 32].into(),
                    index: 0,
                },
                signature_script: vec![],
                sequence: 0,
                sig_op_count: 0,
                verbose_data: None,
            }],
            outputs: vec![RpcTransactionOutput {
                value: 0,
                script_public_key: empty_spk,
                verbose_data: None,
            }],
            lock_time: 0,
            subnetwork_id: RpcSubnetworkId::from_byte(0),
            gas: 0,
            payload,
            mass: 0,
            verbose_data: Some(RpcTransactionVerboseData {
                transaction_id: id.into(),
                hash: id.into(),
                compute_mass: 0,
                block_hash: [0u8; 32].into(),
                block_time: 0,
            }),
        }
    }

    /// The load-bearing regression test for the 2026-08-04 diagnosis: a 97b1 tx
    /// in a mergeset-red block (never chain-accepted, never in
    /// accepted_transaction_ids) MUST still be collected. The old
    /// `drain_matching_txs` path silently dropped these, causing ~20% of L1→L2
    /// linkages to go missing forever.
    #[test]
    fn collect_prefix_txs_captures_non_accepted_mergeset_tx() {
        let parser = parser();
        let mut visited: HashSet<[u8; 32]> = HashSet::new();
        let mut out: Vec<AcceptedTx> = Vec::new();

        // Simulate a mergeset-red block containing a 97b1 tx that was never
        // in any chain block's accepted_transaction_ids.
        let mergeset_red_txs = vec![make_tx(&[0x97, 0xb1, 0xff], b"payload".to_vec())];
        collect_prefix_txs_from_slice(&mergeset_red_txs, &parser, &mut visited, &mut out);

        assert_eq!(out.len(), 1, "the non-accepted mergeset 97b1 tx MUST be collected");
        assert!(out[0].txid.starts_with(&[0x97, 0xb1]));
    }

    #[test]
    fn collect_prefix_txs_filters_non_igra() {
        let parser = parser();
        let mut visited: HashSet<[u8; 32]> = HashSet::new();
        let mut out: Vec<AcceptedTx> = Vec::new();

        let txs = vec![
            make_tx(&[0x97, 0xb1, 0x01], b"igra-1".to_vec()),
            make_tx(&[0x42, 0x00, 0x00], b"not-igra".to_vec()), // random Kaspa tx
            make_tx(&[0x97, 0xb1, 0x02], b"igra-2".to_vec()),
        ];
        collect_prefix_txs_from_slice(&txs, &parser, &mut visited, &mut out);

        assert_eq!(out.len(), 2, "only 97b1 txs should land");
        assert!(out.iter().all(|t| t.txid.starts_with(&[0x97, 0xb1])));
    }

    #[test]
    fn collect_prefix_txs_dedupes_across_chain_body_and_mergeset() {
        let parser = parser();
        let mut visited: HashSet<[u8; 32]> = HashSet::new();
        let mut out: Vec<AcceptedTx> = Vec::new();

        // Same tx appearing in chain block body + a mergeset block (common in
        // Kaspa DAG — a tx can be included in multiple concurrent blocks).
        let chain_body = vec![make_tx(&[0x97, 0xb1, 0xAA], b"payload".to_vec())];
        let mergeset_body = vec![make_tx(&[0x97, 0xb1, 0xAA], b"payload".to_vec())];

        collect_prefix_txs_from_slice(&chain_body, &parser, &mut visited, &mut out);
        collect_prefix_txs_from_slice(&mergeset_body, &parser, &mut visited, &mut out);

        assert_eq!(out.len(), 1, "duplicate visits of the same txid must be deduped");
    }

    #[test]
    fn collect_prefix_txs_skips_txs_without_verbose_data() {
        let parser = parser();
        let mut visited: HashSet<[u8; 32]> = HashSet::new();
        let mut out: Vec<AcceptedTx> = Vec::new();

        let mut tx = make_tx(&[0x97, 0xb1, 0x01], b"payload".to_vec());
        tx.verbose_data = None;
        collect_prefix_txs_from_slice(&[tx], &parser, &mut visited, &mut out);

        assert!(out.is_empty(), "tx without verbose_data has no txid — must skip");
    }
}
