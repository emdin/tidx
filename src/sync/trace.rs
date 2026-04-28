//! Internal-transaction tracing — parses geth-style `callTracer` output and
//! flattens the recursive call tree into one row per internal call.
//!
//! Why "internal txs": when a top-level tx calls a contract that in turn calls
//! another contract (or transfers value), only the top-level tx is visible in
//! `txs`/`receipts`. Internal calls — and especially the value transfers in
//! them — are invisible without trace data.
//!
//! The flattener emits **only nested calls** (depth ≥ 1). The top-level call
//! at depth 0 is the tx itself, already represented in `txs`/`receipts`.
//!
//! Source format documented at
//! <https://geth.ethereum.org/docs/developers/evm-tracing/built-in-tracers#call-tracer>
//! — Igra's reth speaks the same shape via `debug_traceTransaction`.

use anyhow::Result;
use futures::future;
use serde::Deserialize;

use crate::sync::fetcher::RpcClient;
use crate::types::{InternalTxRow, TxRow};

/// One node of the geth `callTracer` output. Recursive: `calls` holds the
/// nested frames. Hex strings are decoded by the flattener so this struct can
/// stay a thin reflection of the JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct CallFrame {
    /// `CALL`, `DELEGATECALL`, `STATICCALL`, `CALLCODE`, `CREATE`, `CREATE2`,
    /// `SELFDESTRUCT`. Kept as a string — small set, stored as TEXT.
    #[serde(rename = "type")]
    pub call_type: String,
    /// Caller, 0x-prefixed 20-byte hex. Always present.
    pub from: String,
    /// Callee. `None` only for unusual frames (e.g. some `CREATE`s that fail
    /// before address derivation). Present for every successful call.
    #[serde(default)]
    pub to: Option<String>,
    /// Value transferred, hex (`0x..`). Defaults to "0x0" if absent.
    #[serde(default)]
    pub value: Option<String>,
    /// Gas given to this frame, hex.
    #[serde(default)]
    pub gas: Option<String>,
    /// Gas consumed by this frame, hex.
    #[serde(rename = "gasUsed", default)]
    pub gas_used: Option<String>,
    /// Calldata, hex. `None` for plain value transfers.
    #[serde(default)]
    pub input: Option<String>,
    /// Return data, hex. `None` for failed calls or void returns.
    #[serde(default)]
    pub output: Option<String>,
    /// EVM-level error message (e.g. `"execution reverted"`, `"out of gas"`).
    /// `None` on success.
    #[serde(default)]
    pub error: Option<String>,
    /// Nested calls in execution order.
    #[serde(default)]
    pub calls: Vec<CallFrame>,
}

/// Flatten a `callTracer` tree into a flat list of `InternalTxRow`s, one per
/// nested call (depth ≥ 1). Pre-order DFS so `path_idx` reflects execution
/// order. The top-level frame (the tx itself) is NOT emitted.
pub fn flatten_call_frame(
    block_num: i64,
    block_timestamp: chrono::DateTime<chrono::Utc>,
    tx_idx: i32,
    tx_hash: &[u8; 32],
    top: &CallFrame,
) -> Result<Vec<InternalTxRow>> {
    let mut out = Vec::new();
    let mut path_idx: i32 = 0;
    for child in &top.calls {
        flatten_recursive(
            block_num,
            block_timestamp,
            tx_idx,
            tx_hash,
            child,
            1,
            &mut path_idx,
            &mut out,
        )?;
    }
    Ok(out)
}

fn flatten_recursive(
    block_num: i64,
    block_timestamp: chrono::DateTime<chrono::Utc>,
    tx_idx: i32,
    tx_hash: &[u8; 32],
    frame: &CallFrame,
    depth: i32,
    path_idx: &mut i32,
    out: &mut Vec<InternalTxRow>,
) -> Result<()> {
    let from = decode_address(&frame.from)?;
    let to = match &frame.to {
        Some(t) => Some(decode_address(t)?),
        None => None,
    };
    let value = match frame.value.as_deref() {
        Some(v) => hex_uint_to_decimal(v)?,
        None => "0".to_string(),
    };
    let input = match frame.input.as_deref() {
        Some(s) => decode_hex_bytes(s)?,
        None => Vec::new(),
    };
    let input_selector = if input.len() >= 4 {
        Some(input[..4].to_vec())
    } else {
        None
    };
    let output = match frame.output.as_deref() {
        Some(s) => decode_hex_bytes(s)?,
        None => Vec::new(),
    };
    let gas_used = match frame.gas_used.as_deref() {
        Some(g) => hex_to_i64(g)?,
        None => 0,
    };

    out.push(InternalTxRow {
        block_num,
        block_timestamp,
        tx_idx,
        tx_hash: tx_hash.to_vec(),
        depth,
        path_idx: *path_idx,
        call_type: frame.call_type.clone(),
        from,
        to,
        value,
        input,
        input_selector,
        output,
        gas_used,
        error: frame.error.clone(),
    });
    *path_idx += 1;

    for child in &frame.calls {
        flatten_recursive(
            block_num,
            block_timestamp,
            tx_idx,
            tx_hash,
            child,
            depth + 1,
            path_idx,
            out,
        )?;
    }
    Ok(())
}

/// Fetch `debug_traceTransaction` for every tx in `txs` (concurrently, capped
/// by the RpcClient's semaphore) and flatten each result into a list of
/// `InternalTxRow`s. The returned vec is the concatenation of per-tx results
/// in the same order as `txs`.
///
/// A single tx that fails to trace (e.g. rate-limited, or the RPC dropped the
/// archive state for that block) does NOT abort the whole batch — it's logged
/// and skipped. The caller can re-run the trace backfill CLI to fill gaps.
pub async fn fetch_and_flatten_traces(
    rpc: &RpcClient,
    txs: &[TxRow],
) -> Result<Vec<InternalTxRow>> {
    if txs.is_empty() {
        return Ok(Vec::new());
    }

    // Fire all per-tx trace calls; the RpcClient semaphore caps concurrency so
    // we don't have to worry about overwhelming the upstream.
    let futs = txs.iter().map(|tx| async move {
        let hash_hex = format!("0x{}", hex::encode(&tx.hash));
        let frame = rpc.trace_transaction(&hash_hex).await?;
        let tx_hash_arr: [u8; 32] = tx
            .hash
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("tx hash is not 32 bytes for {hash_hex}"))?;
        flatten_call_frame(
            tx.block_num,
            tx.block_timestamp,
            tx.idx,
            &tx_hash_arr,
            &frame,
        )
    });

    let results = future::join_all(futs).await;
    let mut out = Vec::new();
    for (i, r) in results.into_iter().enumerate() {
        match r {
            Ok(rows) => out.extend(rows),
            Err(e) => {
                tracing::warn!(
                    tx_idx = txs[i].idx,
                    block_num = txs[i].block_num,
                    error = %e,
                    "Failed to trace tx; skipping"
                );
            }
        }
    }
    Ok(out)
}

/// Decode a `0x`-prefixed 20-byte hex string into `Vec<u8>` length 20. Bad
/// input errors loudly — the tracer should never emit malformed addresses,
/// and silently storing junk would corrupt downstream queries.
fn decode_address(s: &str) -> Result<Vec<u8>> {
    let bytes = decode_hex_bytes(s)?;
    if bytes.len() != 20 {
        return Err(anyhow::anyhow!(
            "address has {} bytes, expected 20: {}",
            bytes.len(),
            s
        ));
    }
    Ok(bytes)
}

/// Decode a `0x`-prefixed hex string into bytes. Empty input (`"0x"` or `""`)
/// yields an empty `Vec`.
fn decode_hex_bytes(s: &str) -> Result<Vec<u8>> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    Ok(hex::decode(trimmed)?)
}

/// Convert a `0x`-prefixed hex uint (any width up to 256 bits) into a decimal
/// string. Used for `value` / wei amounts which can exceed u64.
fn hex_uint_to_decimal(s: &str) -> Result<String> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    if trimmed.is_empty() || trimmed == "0" {
        return Ok("0".to_string());
    }
    let padded: std::borrow::Cow<str> = if trimmed.len() % 2 == 1 {
        std::borrow::Cow::Owned(format!("0{trimmed}"))
    } else {
        std::borrow::Cow::Borrowed(trimmed)
    };
    let bytes = hex::decode(padded.as_ref())?;
    let value = alloy::primitives::U256::from_be_slice(&bytes);
    Ok(value.to_string())
}

/// Convert a `0x`-prefixed hex uint into i64 (gas-sized). Saturates at
/// `i64::MAX` rather than overflowing — defensive for unexpectedly large
/// values that shouldn't realistically come from a tracer.
fn hex_to_i64(s: &str) -> Result<i64> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    if trimmed.is_empty() {
        return Ok(0);
    }
    Ok(u64::from_str_radix(trimmed, 16).map(|v| v.min(i64::MAX as u64) as i64)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
    }

    fn tx_hash() -> [u8; 32] {
        [0x42; 32]
    }

    fn addr(byte: u8) -> String {
        format!("0x{}", hex::encode(vec![byte; 20]))
    }

    fn leaf_call(from: u8, to: u8, value_hex: &str) -> CallFrame {
        CallFrame {
            call_type: "CALL".into(),
            from: addr(from),
            to: Some(addr(to)),
            value: Some(value_hex.into()),
            gas: Some("0x100000".into()),
            gas_used: Some("0x5208".into()),
            input: None,
            output: None,
            error: None,
            calls: vec![],
        }
    }

    // ---------- pure flatten behavior ----------

    #[test]
    fn flatten_skips_top_level_call() {
        // A tx with no internal calls produces zero rows — top level is the
        // tx itself, already in txs/receipts.
        let top = leaf_call(0xaa, 0xbb, "0x0");
        let rows = flatten_call_frame(100, ts(), 0, &tx_hash(), &top).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn flatten_emits_one_row_per_nested_call_at_correct_depth() {
        let top = CallFrame {
            calls: vec![leaf_call(0xbb, 0xcc, "0xde0b6b3a7640000")], // 1 ETH
            ..leaf_call(0xaa, 0xbb, "0x0")
        };
        let rows = flatten_call_frame(100, ts(), 0, &tx_hash(), &top).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].depth, 1);
        assert_eq!(rows[0].path_idx, 0);
        assert_eq!(rows[0].call_type, "CALL");
        assert_eq!(rows[0].from, vec![0xbb; 20]);
        assert_eq!(rows[0].to, Some(vec![0xcc; 20]));
        assert_eq!(rows[0].value, "1000000000000000000");
        assert_eq!(rows[0].block_num, 100);
        assert_eq!(rows[0].tx_idx, 0);
        assert_eq!(rows[0].tx_hash, vec![0x42; 32]);
    }

    #[test]
    fn flatten_dfs_order_with_path_idx_increment() {
        // Tree:
        //   top
        //   ├── A (depth 1, path 0)
        //   │   └── A1 (depth 2, path 1)
        //   └── B (depth 1, path 2)
        let top = CallFrame {
            calls: vec![
                CallFrame {
                    calls: vec![leaf_call(0xa1, 0xa2, "0x0")],
                    ..leaf_call(0xaa, 0xa1, "0x0")
                },
                leaf_call(0xbb, 0xb1, "0x0"),
            ],
            ..leaf_call(0x00, 0xaa, "0x0")
        };
        let rows = flatten_call_frame(100, ts(), 0, &tx_hash(), &top).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[0].depth, rows[0].path_idx), (1, 0));
        assert_eq!(rows[0].from, vec![0xaa; 20]);
        assert_eq!((rows[1].depth, rows[1].path_idx), (2, 1));
        assert_eq!(rows[1].from, vec![0xa1; 20]);
        assert_eq!((rows[2].depth, rows[2].path_idx), (1, 2));
        assert_eq!(rows[2].from, vec![0xbb; 20]);
    }

    #[test]
    fn flatten_extracts_input_selector_from_calldata() {
        let top = CallFrame {
            calls: vec![CallFrame {
                input: Some("0xa9059cbb000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
                ..leaf_call(0xbb, 0xcc, "0x0")
            }],
            ..leaf_call(0xaa, 0xbb, "0x0")
        };
        let rows = flatten_call_frame(100, ts(), 0, &tx_hash(), &top).unwrap();
        assert_eq!(rows[0].input_selector, Some(vec![0xa9, 0x05, 0x9c, 0xbb]));
        assert_eq!(rows[0].input.len(), 36); // 4 + 32
    }

    #[test]
    fn flatten_input_too_short_yields_no_selector() {
        let top = CallFrame {
            calls: vec![CallFrame {
                input: Some("0x01".into()),
                ..leaf_call(0xbb, 0xcc, "0x0")
            }],
            ..leaf_call(0xaa, 0xbb, "0x0")
        };
        let rows = flatten_call_frame(100, ts(), 0, &tx_hash(), &top).unwrap();
        assert_eq!(rows[0].input_selector, None);
        assert_eq!(rows[0].input, vec![0x01]);
    }

    #[test]
    fn flatten_carries_error_on_failed_calls() {
        let top = CallFrame {
            calls: vec![CallFrame {
                error: Some("execution reverted".into()),
                ..leaf_call(0xbb, 0xcc, "0x0")
            }],
            ..leaf_call(0xaa, 0xbb, "0x0")
        };
        let rows = flatten_call_frame(100, ts(), 0, &tx_hash(), &top).unwrap();
        assert_eq!(rows[0].error.as_deref(), Some("execution reverted"));
    }

    #[test]
    fn flatten_create_with_no_to_is_allowed() {
        // CREATE frames may have no `to` (especially if they revert before
        // contract deployment).
        let top = CallFrame {
            calls: vec![CallFrame {
                call_type: "CREATE".into(),
                to: None,
                ..leaf_call(0xaa, 0x00, "0x0")
            }],
            ..leaf_call(0xaa, 0xbb, "0x0")
        };
        let rows = flatten_call_frame(100, ts(), 0, &tx_hash(), &top).unwrap();
        assert_eq!(rows[0].call_type, "CREATE");
        assert_eq!(rows[0].to, None);
    }

    // ---------- low-level helpers ----------

    #[test]
    fn hex_uint_handles_zero_and_full_uint256() {
        assert_eq!(hex_uint_to_decimal("0x0").unwrap(), "0");
        assert_eq!(hex_uint_to_decimal("0x").unwrap(), "0");
        assert_eq!(hex_uint_to_decimal("0xde0b6b3a7640000").unwrap(), "1000000000000000000");
        assert_eq!(hex_uint_to_decimal("0xffffffffffffffff").unwrap(), "18446744073709551615");
        assert_eq!(
            hex_uint_to_decimal("0x10000000000000000").unwrap(),
            "18446744073709551616"
        );
    }

    #[test]
    fn hex_uint_handles_odd_length() {
        // "0x1" would fail naive hex::decode without padding; we pad.
        assert_eq!(hex_uint_to_decimal("0x1").unwrap(), "1");
        assert_eq!(hex_uint_to_decimal("0xabc").unwrap(), "2748");
    }

    #[test]
    fn decode_address_validates_length() {
        let valid = format!("0x{}", "11".repeat(20));
        assert_eq!(decode_address(&valid).unwrap(), vec![0x11; 20]);

        let too_short = "0x1234";
        assert!(decode_address(too_short).is_err());

        let too_long = format!("0x{}", "11".repeat(21));
        assert!(decode_address(&too_long).is_err());
    }

    #[test]
    fn hex_to_i64_clamps_extreme_values() {
        assert_eq!(hex_to_i64("0x0").unwrap(), 0);
        assert_eq!(hex_to_i64("0x5208").unwrap(), 21000);
        // u64::MAX clamps to i64::MAX (rather than wrapping or erroring)
        assert_eq!(hex_to_i64("0xffffffffffffffff").unwrap(), i64::MAX);
    }

    // ---------- end-to-end JSON parse + flatten ----------

    #[test]
    fn parse_and_flatten_real_tracer_output_shape() {
        let json = r#"{
            "type": "CALL",
            "from": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "to": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "value": "0x0",
            "gas": "0x100000",
            "gasUsed": "0x5208",
            "input": "0xa9059cbb",
            "calls": [
                {
                    "type": "DELEGATECALL",
                    "from": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "to": "0xcccccccccccccccccccccccccccccccccccccccc",
                    "gas": "0x80000",
                    "gasUsed": "0x1234",
                    "input": "0xdeadbeef"
                }
            ]
        }"#;
        let frame: CallFrame = serde_json::from_str(json).unwrap();
        let rows = flatten_call_frame(100, ts(), 0, &tx_hash(), &frame).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].call_type, "DELEGATECALL");
        assert_eq!(rows[0].from, vec![0xbb; 20]);
        assert_eq!(rows[0].to, Some(vec![0xcc; 20]));
        assert_eq!(rows[0].value, "0"); // missing value field defaults to "0"
        assert_eq!(rows[0].input_selector, Some(vec![0xde, 0xad, 0xbe, 0xef]));
    }
}
