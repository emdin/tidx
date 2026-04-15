use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use kaspa_rpc_core::{RpcDataVerbosityLevel, api::rpc::RpcApi};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::config::{ChainConfig, KaspaConfig};
use crate::kaspa::clickhouse::KaspaClickHouseMirror;
use crate::kaspa::client::connect_borsh_wrpc;
use crate::kaspa::payload::{IgraKaspaPayload, IgraPayloadParser};
use crate::kaspa::writer::{KaspaProvenanceWriter, PendingEntry, PendingL2Submission};

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
    info!(
        chain_id = chain.chain_id,
        kaspa_rpc = %kaspa.rpc_url,
        kaspa_version = %server_info.server_version,
        kaspa_network = %server_info.network_id,
        txid_prefix = %parser.txid_prefix_hex(),
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

        let response = client
            .get_virtual_chain_from_block_v2(
                checkpoint.into(),
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

        if response.removed_chain_block_hashes.is_empty()
            && response.added_chain_block_hashes.is_empty()
        {
            mirror_promotions(&writer, clickhouse.as_ref()).await?;
            continue;
        }

        let removed_hashes = response
            .removed_chain_block_hashes
            .iter()
            .map(|h| h.as_bytes())
            .collect::<Vec<_>>();
        let deleted = writer
            .delete_pending_for_removed_blocks(&removed_hashes)
            .await?;
        if deleted > 0 {
            tip_distance = tip_distance.saturating_add(1);
            warn!(
                chain_id = chain.chain_id,
                deleted,
                tip_distance,
                "Kaspa provenance removed pending rows after virtual-chain reorg"
            );
        }

        let (pending_l2, pending_entries) = extract_pending_rows(&response, parser.as_ref())?;
        writer.insert_pending(&pending_l2, &pending_entries).await?;
        mirror_promotions(&writer, clickhouse.as_ref()).await?;

        if let Some(last) = response.added_chain_block_hashes.last() {
            checkpoint = last.as_bytes();
            writer
                .update_success(
                    &checkpoint,
                    &dag_info.sink.as_bytes(),
                    response
                        .chain_block_accepted_transactions
                        .last()
                        .and_then(|group| group.chain_block_header.daa_score),
                    tip_distance,
                )
                .await?;
        }

        debug!(
            chain_id = chain.chain_id,
            pending_l2 = pending_l2.len(),
            pending_entries = pending_entries.len(),
            added = response.added_chain_block_hashes.len(),
            removed = response.removed_chain_block_hashes.len(),
            "Kaspa provenance batch processed"
        );
    }
}

fn extract_pending_rows(
    response: &kaspa_rpc_core::GetVirtualChainFromBlockV2Response,
    parser: &IgraPayloadParser,
) -> Result<(Vec<PendingL2Submission>, Vec<PendingEntry>)> {
    let mut l2_submissions = Vec::new();
    let mut entries = Vec::new();

    for group in response.chain_block_accepted_transactions.iter() {
        let block_hash = group
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
            let kaspa_txid = txid.as_bytes();
            match parser.parse(&kaspa_txid, payload)? {
                Some(IgraKaspaPayload::L2Submission { l2_tx_hash }) => {
                    l2_submissions.push(PendingL2Submission {
                        l2_tx_hash,
                        kaspa_txid,
                        accepted_chain_block_hash: block_hash,
                        accepted_at,
                    });
                }
                Some(IgraKaspaPayload::Entry {
                    recipient,
                    amount_sompi,
                }) => {
                    entries.push(PendingEntry {
                        kaspa_txid,
                        recipient,
                        amount_sompi,
                        accepted_chain_block_hash: block_hash,
                        accepted_at,
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
