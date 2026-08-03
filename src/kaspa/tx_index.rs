//! `kaspa_tx_index` — the (txid → block_hash) mapping the [`fee::FeeResolver`]
//! consults to walk previous-outpoint amounts.
//!
//! Two entry points, both idempotent:
//!  - Realtime (in `sync.rs`): every block we process, upsert its full tx
//!    list.
//!  - Backfill (`tidx backfill-kaspa-tx-index`): walk kaspad backward for
//!    the retention window.
//!
//! Both go through [`upsert_batch`]. Rows are constructed by [`extract_rows`]
//! from an RPC block — a pure function so the batching path is unit-testable
//! without hitting kaspad.
//!
//! Design notes:
//!
//! - **PK is `txid`, not `(txid, block_hash)`.** A Kaspa tx can appear in
//!   multiple concurrent blocks (chain block + mergeset blocks all embed
//!   the same tx body). For fee resolution any of these blocks works —
//!   `get_block(hash, true)` on any of them returns the tx list. First seen
//!   wins via `ON CONFLICT DO NOTHING`.
//!
//! - **Skip txs missing `verbose_data`.** Should never happen when
//!   `get_block` is called with `include_transactions=true`, but a defensive
//!   `continue` on the None case keeps a malformed RPC response from
//!   crashing the walk.
//!
//! - **daa_score is block-level, not tx-level** — we pass it in from the
//!   block header once per block. It's optional in the schema (INT8 NULL)
//!   so we can skip it when unknown (e.g. realtime path before verbose
//!   data lands).

use anyhow::{Context, Result};
use kaspa_rpc_core::RpcTransaction;

use crate::db;

/// One row destined for `kaspa_tx_index`. Kept as raw bytes to avoid a
/// dependency on kaspa_hashes at this layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxIndexRow {
    pub txid: [u8; 32],
    pub block_hash: [u8; 32],
    pub daa_score: Option<i64>,
}

/// Project a slice of `RpcTransaction` into `TxIndexRow`s. Every tx whose
/// `verbose_data.transaction_id` is set produces one row. Txs missing
/// verbose_data are silently skipped — they can't be usefully indexed.
///
/// `block_hash` and `daa_score` are copied from the enclosing block into
/// every row (they're block-level, not tx-level).
pub fn extract_rows(
    block_hash: [u8; 32],
    daa_score: Option<i64>,
    txs: &[RpcTransaction],
) -> Vec<TxIndexRow> {
    let mut out = Vec::with_capacity(txs.len());
    for tx in txs {
        if let Some(verbose) = &tx.verbose_data {
            out.push(TxIndexRow {
                txid: verbose.transaction_id.as_bytes(),
                block_hash,
                daa_score,
            });
        }
    }
    out
}

/// Upsert a batch of `TxIndexRow`s into `kaspa_tx_index`. `ON CONFLICT
/// DO NOTHING` on the `txid` PK — first-seen (txid, block_hash) wins.
/// Returns the number of newly-inserted rows (existing rows count as 0).
pub async fn upsert_batch(pool: &db::Pool, rows: &[TxIndexRow]) -> Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }
    let conn = pool.get().await?;

    // Build parallel arrays for UNNEST — one round-trip covers the whole
    // batch. Bytea slices are borrowed from the rows so no allocation
    // beyond the outer Vec.
    let txids: Vec<&[u8]> = rows.iter().map(|r| r.txid.as_slice()).collect();
    let block_hashes: Vec<&[u8]> = rows.iter().map(|r| r.block_hash.as_slice()).collect();
    let daa_scores: Vec<Option<i64>> = rows.iter().map(|r| r.daa_score).collect();

    let n = conn
        .execute(
            "INSERT INTO kaspa_tx_index (txid, block_hash, daa_score)
             SELECT * FROM UNNEST($1::bytea[], $2::bytea[], $3::int8[])
             ON CONFLICT (txid) DO NOTHING",
            &[&txids, &block_hashes, &daa_scores],
        )
        .await
        .context("INSERT kaspa_tx_index batch")?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_rpc_core::{RpcSubnetworkId, RpcTransactionVerboseData};

    fn h(tag: u8) -> [u8; 32] {
        let mut a = [0u8; 32];
        a[0] = tag;
        a
    }

    fn tx_with_verbose(txid_tag: u8) -> RpcTransaction {
        RpcTransaction {
            version: 0,
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            subnetwork_id: RpcSubnetworkId::from_byte(0),
            gas: 0,
            payload: vec![],
            mass: 0,
            verbose_data: Some(RpcTransactionVerboseData {
                transaction_id: h(txid_tag).into(),
                hash: h(txid_tag).into(),
                compute_mass: 0,
                block_hash: h(0xff).into(),
                block_time: 0,
            }),
        }
    }

    fn tx_without_verbose(_tag: u8) -> RpcTransaction {
        RpcTransaction {
            version: 0,
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            subnetwork_id: RpcSubnetworkId::from_byte(0),
            gas: 0,
            payload: vec![],
            mass: 0,
            verbose_data: None,
        }
    }

    #[test]
    fn extract_rows_empty_txs_returns_empty() {
        let rows = extract_rows(h(0x10), Some(42), &[]);
        assert!(rows.is_empty());
    }

    #[test]
    fn extract_rows_normal_block_produces_row_per_tx() {
        let txs = vec![tx_with_verbose(0xAA), tx_with_verbose(0xBB), tx_with_verbose(0xCC)];
        let rows = extract_rows(h(0x10), Some(1000), &txs);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            TxIndexRow {
                txid: h(0xAA),
                block_hash: h(0x10),
                daa_score: Some(1000),
            }
        );
        assert_eq!(rows[2].txid, h(0xCC));
        assert!(rows.iter().all(|r| r.block_hash == h(0x10)));
        assert!(rows.iter().all(|r| r.daa_score == Some(1000)));
    }

    #[test]
    fn extract_rows_skips_txs_without_verbose_data() {
        // Should never happen in production (`get_block(_, true)` always
        // populates verbose_data), but a defensive skip prevents a
        // malformed RPC response from crashing the walk.
        let txs = vec![
            tx_with_verbose(0xAA),
            tx_without_verbose(0xBB),
            tx_with_verbose(0xCC),
        ];
        let rows = extract_rows(h(0x10), None, &txs);
        assert_eq!(rows.len(), 2, "0xBB (no verbose) must be dropped");
        assert_eq!(rows[0].txid, h(0xAA));
        assert_eq!(rows[1].txid, h(0xCC));
    }

    #[test]
    fn extract_rows_none_daa_score_passes_through() {
        let txs = vec![tx_with_verbose(0xAA)];
        let rows = extract_rows(h(0x10), None, &txs);
        assert_eq!(rows[0].daa_score, None);
    }
}
