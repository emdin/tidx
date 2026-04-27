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
            fetch_chain_update_v2(&client, &checkpoint, tip_distance).await?
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

/// Drain `wanted` of any txids found in the provided iterator, appending matching
/// (txid, payload) pairs to `out`. Items whose txid is `None` (e.g. RpcTransaction
/// without verbose_data) are skipped; the iterator stops early once `wanted` empties.
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

/// Adapter that projects a slice of `RpcTransaction` into the `(Option<txid>, &payload)`
/// view that `drain_matching_txs` consumes. Keeping this thin so tests can construct
/// the same view directly without building full `RpcTransaction` fixtures.
fn rpc_txs_view(
    txs: &[kaspa_rpc_core::RpcTransaction],
) -> impl Iterator<Item = (Option<[u8; 32]>, &[u8])> {
    txs.iter().map(|tx| {
        let txid = tx.verbose_data.as_ref().map(|v| v.transaction_id.as_bytes());
        (txid, tx.payload.as_slice())
    })
}

async fn fetch_chain_update_v2(
    client: &KaspaRpcClient,
    checkpoint: &[u8; 32],
    tip_distance: u64,
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

    let mut added: Vec<AddedChainBlock> = Vec::with_capacity(
        response.chain_block_accepted_transactions.len(),
    );
    for group in response.chain_block_accepted_transactions.iter() {
        let hash = group
            .chain_block_header
            .hash
            .ok_or_else(|| anyhow!("V2 response missing chain block hash"))?
            .as_bytes();
        let accepted_at = group
            .chain_block_header
            .timestamp
            .and_then(|ms| i64::try_from(ms).ok())
            .and_then(DateTime::<Utc>::from_timestamp_millis)
            .unwrap_or_else(Utc::now);
        let mut accepted_transactions: Vec<AcceptedTx> = Vec::new();
        for tx in &group.accepted_transactions {
            let Some(verbose) = &tx.verbose_data else {
                continue;
            };
            let Some(txid) = verbose.transaction_id else {
                continue;
            };
            let Some(payload) = &tx.payload else {
                continue;
            };
            accepted_transactions.push(AcceptedTx {
                txid: txid.as_bytes(),
                payload: payload.clone(),
            });
        }
        added.push(AddedChainBlock {
            hash,
            accepted_at,
            accepted_transactions,
        });
    }

    let last_daa_score = response
        .chain_block_accepted_transactions
        .last()
        .and_then(|g| g.chain_block_header.daa_score);

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

    // Prefix filtering happens here (not later) so we can skip the expensive
    // get_block round-trips for chain blocks with no Igra-relevant activity.
    let prefix_accepted_by_block = group_prefixed_accepted_txids(&response, parser);

    let added_hashes: Vec<[u8; 32]> = response
        .added_chain_block_hashes
        .iter()
        .map(|h| h.as_bytes())
        .collect();

    let last_hash = added_hashes.last().copied();
    let mut added: Vec<AddedChainBlock> = Vec::with_capacity(added_hashes.len());
    let mut last_daa_score: Option<u64> = None;

    for block_hash in &added_hashes {
        let accepted_txid_set = prefix_accepted_by_block.get(block_hash);
        let is_last = Some(*block_hash) == last_hash;

        if accepted_txid_set.is_none() && !is_last {
            // No Igra-prefixed accepted txs — skip the RPC, no pending rows will result.
            added.push(AddedChainBlock {
                hash: *block_hash,
                accepted_at: Utc::now(),
                accepted_transactions: Vec::new(),
            });
            continue;
        }

        let chain_block = client
            .get_block((*block_hash).into(), true)
            .await
            .with_context(|| {
                format!(
                    "get_block failed for chain block {}",
                    hex::encode(block_hash)
                )
            })?;

        let accepted_at = i64::try_from(chain_block.header.timestamp)
            .ok()
            .and_then(DateTime::<Utc>::from_timestamp_millis)
            .unwrap_or_else(Utc::now);
        if is_last {
            last_daa_score = Some(chain_block.header.daa_score);
        }

        let mut accepted_transactions: Vec<AcceptedTx> = Vec::new();
        if let Some(txid_list) = accepted_txid_set {
            let mut remaining: HashSet<[u8; 32]> = txid_list.iter().copied().collect();

            drain_matching_txs(
                rpc_txs_view(&chain_block.transactions),
                &mut remaining,
                &mut accepted_transactions,
            );

            if !remaining.is_empty() {
                if let Some(verbose) = &chain_block.verbose_data {
                    let mergeset_hashes: Vec<_> = verbose
                        .merge_set_blues_hashes
                        .iter()
                        .chain(verbose.merge_set_reds_hashes.iter())
                        .copied()
                        .collect();
                    for mh in mergeset_hashes {
                        if remaining.is_empty() {
                            break;
                        }
                        let merged = client
                            .get_block(mh, true)
                            .await
                            .with_context(|| {
                                format!(
                                    "get_block failed for merged block {}",
                                    hex::encode(mh.as_bytes())
                                )
                            })?;
                        drain_matching_txs(
                            rpc_txs_view(&merged.transactions),
                            &mut remaining,
                            &mut accepted_transactions,
                        );
                    }
                }
            }

            if !remaining.is_empty() {
                warn!(
                    chain_block = %hex::encode(block_hash),
                    missing = remaining.len(),
                    "Kaspa v1 fallback: accepted Igra txs not found in chain block or mergeset"
                );
            }
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
}
