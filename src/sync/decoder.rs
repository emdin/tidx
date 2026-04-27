use alloy::consensus::{BlockHeader, Transaction as TransactionTrait, Typed2718};
use alloy::network::{ReceiptResponse, TransactionResponse};
use chrono::{DateTime, TimeZone, Utc};

use crate::igra_timestamp::decode_igra_timestamp_metadata;
use crate::tempo::{Block, Log, Receipt, Transaction};
use crate::types::{BlockRow, L2WithdrawalRow, LogRow, ReceiptRow, TxRow};

pub fn timestamp_from_secs(secs: u64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs as i64, 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().unwrap())
}

pub fn decode_block(block: &Block) -> BlockRow {
    let timestamp_secs = block.header.timestamp;
    let timestamp = timestamp_from_secs(timestamp_secs);
    let timestamp_ms = (timestamp_secs * 1000) as i64;
    let parent_beacon_block_root = block
        .parent_beacon_block_root
        .or_else(|| block.header.parent_beacon_block_root())
        .map(|root| root.as_slice().to_vec());
    let igra_timestamp = parent_beacon_block_root
        .as_deref()
        .and_then(|root| decode_igra_timestamp_metadata(root, timestamp_secs));

    BlockRow {
        num: block.header.number as i64,
        hash: block.header.hash.as_slice().to_vec(),
        parent_hash: block.header.parent_hash.as_slice().to_vec(),
        timestamp,
        timestamp_ms,
        real_timestamp: igra_timestamp.as_ref().map(|m| m.real_timestamp),
        real_timestamp_ms: igra_timestamp.as_ref().map(|m| m.real_timestamp_ms),
        timestamp_drift_secs: igra_timestamp.as_ref().map(|m| m.timestamp_drift_secs),
        l1_block_count: igra_timestamp.as_ref().map(|m| m.l1_block_count),
        l1_last_daa_score: igra_timestamp.as_ref().map(|m| m.l1_last_daa_score),
        parent_beacon_block_root,
        gas_limit: block.header.gas_limit as i64,
        gas_used: block.header.gas_used as i64,
        miner: block.header.beneficiary.as_slice().to_vec(),
        extra_data: Some(block.header.extra_data.to_vec()),
    }
}

pub fn decode_transaction(tx: &Transaction, block: &Block, idx: u32) -> TxRow {
    let block_timestamp = timestamp_from_secs(block.header.timestamp);
    let input = tx.input().to_vec();
    let selector = selector_from_input(&input);

    TxRow {
        block_num: block.header.number as i64,
        block_timestamp,
        idx: idx as i32,
        hash: tx.tx_hash().as_slice().to_vec(),
        tx_type: tx.ty() as i16,
        from: tx.from().as_slice().to_vec(),
        to: tx.to().map(|a| a.as_slice().to_vec()),
        value: tx.value().to_string(),
        input,
        selector,
        gas_limit: tx.gas_limit() as i64,
        max_fee_per_gas: TransactionTrait::max_fee_per_gas(tx).to_string(),
        max_priority_fee_per_gas: TransactionTrait::max_priority_fee_per_gas(tx)
            .map_or("0".into(), |v| v.to_string()),
        gas_used: None,
        // Preserve the existing schema even for generic EVM chains. These
        // columns remain empty/default unless the upstream chain exposes
        // compatible extended transaction metadata.
        nonce_key: vec![0u8; 32],
        nonce: tx.nonce() as i64,
        fee_token: None,
        fee_payer: None, // Recovered from receipt
        calls: None,
        call_count: 1,
        valid_before: None,
        valid_after: None,
        signature_type: None,
    }
}

pub fn decode_withdrawals(block: &Block) -> Vec<L2WithdrawalRow> {
    let block_timestamp = timestamp_from_secs(block.header.timestamp);
    let Some(withdrawals) = block.withdrawals.as_ref() else {
        return Vec::new();
    };

    withdrawals
        .iter()
        .enumerate()
        .map(|(idx, withdrawal)| {
            let amount_gwei = i64::try_from(withdrawal.amount).unwrap_or(i64::MAX);
            L2WithdrawalRow {
                block_num: block.header.number as i64,
                block_timestamp,
                idx: idx as i32,
                withdrawal_index: withdrawal.index.to_string(),
                index_le: withdrawal.index.to_le_bytes().to_vec(),
                validator_index: withdrawal.validator_index.to_string(),
                address: withdrawal.address.as_slice().to_vec(),
                amount_gwei,
                // Igra entry amounts are sompi. The Engine API withdrawal field is gwei,
                // so the adapter multiplies sompi by 10 before block inclusion.
                amount_sompi: amount_gwei / 10,
            }
        })
        .collect()
}

pub fn decode_log(log: &Log, block_timestamp: DateTime<Utc>) -> LogRow {
    let topics = log.topics();
    let selector = topics.first().map(|s| s.as_slice().to_vec());

    LogRow {
        block_num: log.block_number.unwrap_or(0) as i64,
        block_timestamp,
        log_idx: log.log_index.unwrap_or(0) as i32,
        tx_idx: log.transaction_index.unwrap_or(0) as i32,
        tx_hash: log
            .transaction_hash
            .map(|h| h.as_slice().to_vec())
            .unwrap_or_default(),
        address: log.address().as_slice().to_vec(),
        selector,
        topic0: topics.first().map(|t| t.as_slice().to_vec()),
        topic1: topics.get(1).map(|t| t.as_slice().to_vec()),
        topic2: topics.get(2).map(|t| t.as_slice().to_vec()),
        topic3: topics.get(3).map(|t| t.as_slice().to_vec()),
        data: log.data().data.to_vec(),
        // Filled by `enrich_logs_from_txs` once parent txs are decoded.
        from: None,
    }
}

/// Returns the first 4 bytes of `input` as the ABI function selector, or `None`
/// when input is shorter than 4 bytes (plain value transfers, etc.). Stored on
/// `TxRow.selector` so the public query API can index on selector without
/// resorting to an expression index.
pub fn selector_from_input(input: &[u8]) -> Option<Vec<u8>> {
    if input.len() >= 4 {
        Some(input[..4].to_vec())
    } else {
        None
    }
}

/// Denormalize `from` from each parent transaction onto its emitted logs. Logs
/// are joined to txs by `(block_num, tx_idx)`; logs whose parent isn't in the
/// supplied slice are left with `from = None`. Mirrors `enrich_txs_from_receipts`.
pub fn enrich_logs_from_txs(logs: &mut [LogRow], txs: &[TxRow]) {
    use std::collections::HashMap;
    let from_map: HashMap<(i64, i32), &[u8]> = txs
        .iter()
        .map(|t| ((t.block_num, t.idx), t.from.as_slice()))
        .collect();
    for log in logs.iter_mut() {
        if let Some(from) = from_map.get(&(log.block_num, log.tx_idx)) {
            log.from = Some(from.to_vec());
        }
    }
}

/// Enrich transaction rows with fields that come from receipts (gas_used, fee_payer).
/// Must be called after both txs and receipts are decoded.
pub fn enrich_txs_from_receipts(txs: &mut [TxRow], receipts: &[ReceiptRow]) {
    use std::collections::HashMap;
    let receipt_map: HashMap<(i64, i32), &ReceiptRow> = receipts
        .iter()
        .map(|r| ((r.block_num, r.tx_idx), r))
        .collect();
    for tx in txs.iter_mut() {
        if let Some(r) = receipt_map.get(&(tx.block_num, tx.idx)) {
            tx.gas_used = Some(r.gas_used);
            tx.fee_payer = r.fee_payer.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tx(block_num: i64, idx: i32) -> TxRow {
        TxRow {
            block_num,
            idx,
            ..Default::default()
        }
    }

    fn make_receipt(
        block_num: i64,
        tx_idx: i32,
        gas_used: i64,
        fee_payer: Option<Vec<u8>>,
    ) -> ReceiptRow {
        ReceiptRow {
            block_num,
            tx_idx,
            gas_used,
            fee_payer,
            ..Default::default()
        }
    }

    #[test]
    fn enrich_sets_gas_used_and_fee_payer() {
        let mut txs = vec![make_tx(1, 0), make_tx(1, 1)];
        let receipts = vec![
            make_receipt(1, 0, 21000, Some(vec![0xaa; 20])),
            make_receipt(1, 1, 50000, Some(vec![0xbb; 20])),
        ];

        enrich_txs_from_receipts(&mut txs, &receipts);

        assert_eq!(txs[0].gas_used, Some(21000));
        assert_eq!(txs[0].fee_payer, Some(vec![0xaa; 20]));
        assert_eq!(txs[1].gas_used, Some(50000));
        assert_eq!(txs[1].fee_payer, Some(vec![0xbb; 20]));
    }

    #[test]
    fn enrich_leaves_unmatched_txs_as_none() {
        let mut txs = vec![make_tx(1, 0), make_tx(2, 0)];
        let receipts = vec![make_receipt(1, 0, 21000, None)];

        enrich_txs_from_receipts(&mut txs, &receipts);

        assert_eq!(txs[0].gas_used, Some(21000));
        assert_eq!(txs[1].gas_used, None);
        assert_eq!(txs[1].fee_payer, None);
    }

    #[test]
    fn enrich_empty_receipts_is_noop() {
        let mut txs = vec![make_tx(1, 0)];
        enrich_txs_from_receipts(&mut txs, &[]);
        assert_eq!(txs[0].gas_used, None);
    }

    #[test]
    fn enrich_empty_txs_is_noop() {
        let mut txs: Vec<TxRow> = vec![];
        let receipts = vec![make_receipt(1, 0, 21000, None)];
        enrich_txs_from_receipts(&mut txs, &receipts);
        assert!(txs.is_empty());
    }

    #[test]
    fn enrich_multi_block_batch() {
        let mut txs = vec![make_tx(10, 0), make_tx(10, 1), make_tx(11, 0)];
        let receipts = vec![
            make_receipt(10, 0, 21000, Some(vec![0x01; 20])),
            make_receipt(10, 1, 42000, None),
            make_receipt(11, 0, 63000, Some(vec![0x02; 20])),
        ];

        enrich_txs_from_receipts(&mut txs, &receipts);

        assert_eq!(txs[0].gas_used, Some(21000));
        assert_eq!(txs[0].fee_payer, Some(vec![0x01; 20]));
        assert_eq!(txs[1].gas_used, Some(42000));
        assert_eq!(txs[1].fee_payer, None);
        assert_eq!(txs[2].gas_used, Some(63000));
        assert_eq!(txs[2].fee_payer, Some(vec![0x02; 20]));
    }

    // ===== selector_from_input (denormalized 4-byte ABI selector) =====

    #[test]
    fn selector_takes_first_four_bytes_of_input() {
        let input = vec![0xa9, 0x05, 0x9c, 0xbb, 0x00, 0x01, 0x02];
        assert_eq!(selector_from_input(&input), Some(vec![0xa9, 0x05, 0x9c, 0xbb]));
    }

    #[test]
    fn selector_handles_exactly_four_bytes() {
        let input = vec![0xa9, 0x05, 0x9c, 0xbb];
        assert_eq!(selector_from_input(&input), Some(vec![0xa9, 0x05, 0x9c, 0xbb]));
    }

    #[test]
    fn selector_is_none_when_input_too_short() {
        assert_eq!(selector_from_input(&[]), None);
        assert_eq!(selector_from_input(&[0xa9]), None);
        assert_eq!(selector_from_input(&[0xa9, 0x05, 0x9c]), None);
    }

    // ===== enrich_logs_from_txs (denormalize tx.from onto each log) =====

    fn make_log(block_num: i64, tx_idx: i32, log_idx: i32) -> LogRow {
        LogRow {
            block_num,
            tx_idx,
            log_idx,
            ..Default::default()
        }
    }

    fn make_tx_with_from(block_num: i64, idx: i32, from: Vec<u8>) -> TxRow {
        TxRow {
            block_num,
            idx,
            from,
            ..Default::default()
        }
    }

    #[test]
    fn enrich_logs_copies_from_address_from_parent_tx() {
        let txs = vec![
            make_tx_with_from(100, 0, vec![0xaa; 20]),
            make_tx_with_from(100, 1, vec![0xbb; 20]),
        ];
        let mut logs = vec![
            make_log(100, 0, 0),
            make_log(100, 0, 1), // second log of tx 0
            make_log(100, 1, 0),
        ];

        enrich_logs_from_txs(&mut logs, &txs);

        assert_eq!(logs[0].from, Some(vec![0xaa; 20]));
        assert_eq!(logs[1].from, Some(vec![0xaa; 20]));
        assert_eq!(logs[2].from, Some(vec![0xbb; 20]));
    }

    #[test]
    fn enrich_logs_leaves_unmatched_as_none() {
        let txs = vec![make_tx_with_from(100, 0, vec![0xaa; 20])];
        let mut logs = vec![
            make_log(100, 0, 0),
            make_log(100, 99, 0), // no tx with idx 99 in this block
            make_log(999, 0, 0),  // wrong block
        ];

        enrich_logs_from_txs(&mut logs, &txs);

        assert_eq!(logs[0].from, Some(vec![0xaa; 20]));
        assert_eq!(logs[1].from, None);
        assert_eq!(logs[2].from, None);
    }

    #[test]
    fn enrich_logs_handles_empty_inputs() {
        let mut logs: Vec<LogRow> = vec![];
        enrich_logs_from_txs(&mut logs, &[]);
        assert!(logs.is_empty());

        let mut logs = vec![make_log(1, 0, 0)];
        enrich_logs_from_txs(&mut logs, &[]);
        assert_eq!(logs[0].from, None);
    }

    #[test]
    fn enrich_logs_multi_block_batch() {
        let txs = vec![
            make_tx_with_from(10, 0, vec![0x01; 20]),
            make_tx_with_from(10, 1, vec![0x02; 20]),
            make_tx_with_from(11, 0, vec![0x03; 20]),
        ];
        let mut logs = vec![
            make_log(10, 0, 0),
            make_log(10, 1, 0),
            make_log(11, 0, 0),
        ];

        enrich_logs_from_txs(&mut logs, &txs);

        assert_eq!(logs[0].from, Some(vec![0x01; 20]));
        assert_eq!(logs[1].from, Some(vec![0x02; 20]));
        assert_eq!(logs[2].from, Some(vec![0x03; 20]));
    }
}

pub fn decode_receipt(receipt: &Receipt, block_timestamp: DateTime<Utc>) -> ReceiptRow {
    ReceiptRow {
        block_num: receipt.block_number().unwrap_or(0) as i64,
        block_timestamp,
        tx_idx: receipt.transaction_index().unwrap_or(0) as i32,
        tx_hash: receipt.transaction_hash().as_slice().to_vec(),
        from: receipt.from().as_slice().to_vec(),
        to: receipt.to().map(|a| a.as_slice().to_vec()),
        contract_address: receipt.contract_address().map(|a| a.as_slice().to_vec()),
        gas_used: receipt.gas_used() as i64,
        cumulative_gas_used: receipt.cumulative_gas_used() as i64,
        effective_gas_price: Some(receipt.effective_gas_price().to_string()),
        status: if receipt.status() { Some(1) } else { Some(0) },
        // Generic EVM receipts do not expose a distinct fee payer. Use the
        // sender so downstream queries still have a concrete payer identity.
        fee_payer: Some(receipt.from().as_slice().to_vec()),
    }
}
