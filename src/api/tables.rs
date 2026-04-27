//! `GET /tables` — schema metadata for every table the public query API exposes.
//!
//! Hand-curated. The set of tables is small (~12) and rarely changes. Single
//! source of truth for "what tables/columns can I query and what do they mean?",
//! intended to close the discoverability gap that made `l2_withdrawals` and
//! `receipts` invisible to API consumers.

use axum::{Json, extract::State};
use serde::Serialize;

use super::{ApiError, AppState};

#[derive(Serialize)]
pub struct ColumnInfo {
    pub name: &'static str,
    /// SQL-ish type string; informational. Postgres and ClickHouse types are
    /// described together when they differ (e.g., `BYTEA / String`).
    #[serde(rename = "type")]
    pub ty: &'static str,
    pub description: &'static str,
}

#[derive(Serialize)]
pub struct QueryExample {
    pub description: &'static str,
    pub sql: &'static str,
}

#[derive(Serialize)]
pub struct TableInfo {
    pub name: &'static str,
    pub description: &'static str,
    /// Which engines can serve this table via `?engine=`. Most chain data is
    /// mirrored to ClickHouse; operational tables (kaspa_sync_state, etc.) live
    /// only in Postgres.
    pub engines: Vec<&'static str>,
    pub columns: Vec<ColumnInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<QueryExample>,
}

#[derive(Serialize)]
pub struct TablesResponse {
    pub ok: bool,
    pub tables: Vec<TableInfo>,
    pub tips: Vec<&'static str>,
}

/// Return curated metadata for every table the validator allows. The order is
/// "things developers care about most" first — chain data, then derived event
/// data, then provenance/operational tables.
pub fn tables_metadata() -> Vec<TableInfo> {
    let pg_ch = || vec!["postgres", "clickhouse"];
    let pg = || vec!["postgres"];

    vec![
        TableInfo {
            name: "blocks",
            description: "L2 blocks. One row per Igra Network chain block.",
            engines: pg_ch(),
            columns: vec![
                col("num", "INT8", "Block number (primary ordering key)"),
                col("hash", "BYTEA / String", "Block hash, 32 bytes"),
                col("parent_hash", "BYTEA / String", "Parent block hash"),
                col("timestamp", "TIMESTAMPTZ / DateTime64", "Block timestamp (synthetic on Igra; see real_timestamp for L1 wall-clock)"),
                col("timestamp_ms", "INT8", "Block timestamp in milliseconds"),
                col("real_timestamp", "TIMESTAMPTZ", "Kaspa L1 wall-clock timestamp (decoded from parentBeaconBlockRoot)"),
                col("gas_limit", "INT8", "Block gas limit"),
                col("gas_used", "INT8", "Total gas used in block"),
                col("miner", "BYTEA / String", "Block producer address (20 bytes)"),
            ],
            examples: vec![
                QueryExample {
                    description: "10 most recent blocks",
                    sql: "SELECT num, encode(hash,'hex'), timestamp, gas_used FROM blocks ORDER BY num DESC LIMIT 10",
                },
            ],
        },
        TableInfo {
            name: "txs",
            description: "L2 transactions. One row per included transaction.",
            engines: pg_ch(),
            columns: vec![
                col("block_num", "INT8", "Block number containing this tx"),
                col("block_timestamp", "TIMESTAMPTZ", "Block timestamp"),
                col("idx", "INT4", "Transaction position within block"),
                col("hash", "BYTEA / String", "Transaction hash, 32 bytes"),
                col("type", "INT2", "EIP-2718 tx type (0=legacy, 2=EIP-1559, etc.)"),
                col("from", "BYTEA / String", "Sender address (20 bytes). Quote in PG: \"from\" — `from` is reserved"),
                col("to", "BYTEA / String", "Recipient address; NULL for contract creation"),
                col("value", "TEXT / String", "Wei value, uint256 as decimal string. Cast in CH with toUInt256OrZero()"),
                col("input", "BYTEA / String", "Calldata. First 4 bytes = ABI selector"),
                col("gas_limit", "INT8", "Tx gas limit"),
                col("gas_used", "INT8", "Gas actually consumed (NULL until receipt arrives)"),
                col("max_fee_per_gas", "TEXT / String", "EIP-1559 max fee per gas (wei, uint256 string)"),
                col("max_priority_fee_per_gas", "TEXT / String", "EIP-1559 priority fee (wei, uint256 string)"),
                col("nonce", "INT8", "Sender's nonce"),
                col("calls", "JSONB / Nullable(String)", "Reserved for internal-call traces; currently always NULL"),
                col("call_count", "INT2", "Reserved for internal-call traces; currently always 1"),
            ],
            examples: vec![
                QueryExample {
                    description: "All ERC-20 transfer() calls (selector 0xa9059cbb) in the last 1000 blocks (PG)",
                    sql: "SELECT block_num, encode(hash,'hex') FROM txs WHERE substring(input,1,4) = decode('a9059cbb','hex') AND block_num > (SELECT max(num) FROM blocks) - 1000",
                },
                QueryExample {
                    description: "Same in CH (string LIKE works since input is hex-prefixed)",
                    sql: "SELECT block_num, hash FROM txs WHERE input LIKE '0xa9059cbb%' ORDER BY block_num DESC LIMIT 100",
                },
            ],
        },
        TableInfo {
            name: "logs",
            description: "Event logs emitted by L2 transactions. Use ?signature= to filter by event signature without manually hashing topic0.",
            engines: pg_ch(),
            columns: vec![
                col("block_num", "INT8", "Block containing the emitting tx"),
                col("block_timestamp", "TIMESTAMPTZ", "Block timestamp"),
                col("log_idx", "INT4", "Position of this log within the block"),
                col("tx_idx", "INT4", "Index of the emitting tx within the block"),
                col("tx_hash", "BYTEA / String", "Hash of the emitting tx"),
                col("address", "BYTEA / String", "Contract address that emitted the log"),
                col("selector", "BYTEA / String", "topic0 — full 32-byte event signature hash. Use ?signature= for human-friendly filtering"),
                col("topic0", "BYTEA / String", "Same as selector, kept for ergonomics"),
                col("topic1", "BYTEA / String", "Indexed parameter 1 (often `from`)"),
                col("topic2", "BYTEA / String", "Indexed parameter 2 (often `to`)"),
                col("topic3", "BYTEA / String", "Indexed parameter 3"),
                col("data", "BYTEA / String", "Non-indexed event data (ABI-encoded)"),
            ],
            examples: vec![
                QueryExample {
                    description: "Recent ERC-20 Transfer events using ?signature= helper",
                    sql: "SELECT block_num, encode(address,'hex'), encode(topic1,'hex'), encode(topic2,'hex') FROM Transfer ORDER BY block_num DESC LIMIT 100",
                },
                QueryExample {
                    description: "Equivalent without ?signature= — must hash 'Transfer(address,address,uint256)' yourself",
                    sql: "SELECT * FROM logs WHERE selector = decode('ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef','hex')",
                },
            ],
        },
        TableInfo {
            name: "receipts",
            description: "Tx receipts. Use to filter for failed txs (status=0) or compute effective gas spend.",
            engines: pg_ch(),
            columns: vec![
                col("block_num", "INT8", "Block number"),
                col("block_timestamp", "TIMESTAMPTZ", "Block timestamp"),
                col("tx_idx", "INT4", "Tx position within block"),
                col("tx_hash", "BYTEA / String", "Transaction hash"),
                col("from", "BYTEA / String", "Sender (quote in SQL: \"from\")"),
                col("to", "BYTEA / String", "Recipient; NULL for contract creation"),
                col("contract_address", "BYTEA / String", "Created contract address; NULL unless this tx deployed a contract"),
                col("gas_used", "INT8", "Gas consumed by this tx"),
                col("cumulative_gas_used", "INT8", "Cumulative gas in block up through this tx"),
                col("effective_gas_price", "TEXT / String", "Actual gas price paid (wei, uint256 string)"),
                col("status", "INT2", "1 = success, 0 = failure"),
            ],
            examples: vec![
                QueryExample {
                    description: "Failed txs in the last 1000 blocks",
                    sql: "SELECT encode(tx_hash,'hex'), encode(\"from\",'hex'), gas_used FROM receipts WHERE status = 0 AND block_num > (SELECT max(num) FROM blocks) - 1000 ORDER BY block_num DESC LIMIT 100",
                },
            ],
        },
        TableInfo {
            name: "l2_withdrawals",
            description: "L2 withdrawals exiting to Kaspa L1 (Engine API withdrawals field). Stores both EVM gwei and Kaspa sompi amounts.",
            engines: pg_ch(),
            columns: vec![
                col("block_num", "INT8", "Block number containing the withdrawal"),
                col("block_timestamp", "TIMESTAMPTZ", "Block timestamp"),
                col("idx", "INT4", "Withdrawal index within the block"),
                col("withdrawal_index", "TEXT / String", "Withdrawal sequence number (uint64 as text)"),
                col("validator_index", "TEXT / String", "Validator index (uint64 as text)"),
                col("address", "BYTEA / String", "Recipient L2 address"),
                col("amount_gwei", "INT8", "Amount in EVM gwei (1e-9 ETH)"),
                col("amount_sompi", "INT8", "Amount in Kaspa sompi (1e-8 KAS) — what actually exits to L1"),
            ],
            examples: vec![
                QueryExample {
                    description: "Daily withdrawal totals (CH)",
                    sql: "SELECT toStartOfDay(block_timestamp) AS day, count() AS withdrawals, sum(amount_sompi) AS total_sompi FROM l2_withdrawals GROUP BY day ORDER BY day DESC",
                },
            ],
        },
        TableInfo {
            name: "kaspa_l2_submissions",
            description: "Confirmed Kaspa L1 transactions that submitted L2 batches to Igra. Promoted from kaspa_pending_l2_submissions after the configured finality delay (12h on mainnet).",
            engines: pg_ch(),
            columns: vec![
                col("l2_tx_hash", "BYTEA / String", "L2 transaction hash committed by this Kaspa tx"),
                col("kaspa_txid", "BYTEA / String", "Kaspa L1 transaction id (32 bytes, prefixed `97b1...`)"),
                col("accepted_chain_block_hash", "BYTEA / String", "Kaspa chain block that accepted the tx"),
                col("accepted_at", "TIMESTAMPTZ", "When the chain block accepted the tx (Kaspa-side timestamp)"),
                col("created_at", "TIMESTAMPTZ", "When this row was inserted into the final table (post-promotion)"),
            ],
            examples: vec![
                QueryExample {
                    description: "Find the Kaspa txid that committed a given L2 tx",
                    sql: "SELECT encode(kaspa_txid,'hex'), accepted_at FROM kaspa_l2_submissions WHERE l2_tx_hash = decode('<l2-hash-hex>','hex')",
                },
            ],
        },
        TableInfo {
            name: "kaspa_entries",
            description: "Confirmed Kaspa L1 entries (native iKAS deposits to Igra). Promoted from kaspa_pending_entries after finality delay.",
            engines: pg_ch(),
            columns: vec![
                col("kaspa_txid", "BYTEA / String", "Kaspa L1 transaction id"),
                col("recipient", "BYTEA / String", "L2 address receiving the iKAS"),
                col("amount_sompi", "INT8", "Deposit amount in Kaspa sompi"),
                col("accepted_chain_block_hash", "BYTEA / String", "Kaspa chain block that accepted the entry"),
                col("accepted_at", "TIMESTAMPTZ", "Kaspa-side acceptance timestamp"),
                col("created_at", "TIMESTAMPTZ", "When this row was promoted to final"),
            ],
            examples: vec![],
        },
        TableInfo {
            name: "kaspa_pending_l2_submissions",
            description: "Pending Kaspa L2 submissions. Newer than the finality delay; moves to kaspa_l2_submissions after promotion. Useful to see in-flight submissions before finality.",
            engines: pg(),
            columns: vec![
                col("l2_tx_hash", "BYTEA", "L2 tx hash"),
                col("kaspa_txid", "BYTEA", "Kaspa txid"),
                col("accepted_chain_block_hash", "BYTEA", "Kaspa chain block hash"),
                col("accepted_at", "TIMESTAMPTZ", "When accepted on Kaspa"),
                col("promote_after", "TIMESTAMPTZ", "When this row is eligible for promotion (accepted_at + delay)"),
            ],
            examples: vec![],
        },
        TableInfo {
            name: "kaspa_pending_entries",
            description: "Pending Kaspa iKAS entries; moves to kaspa_entries after finality delay.",
            engines: pg(),
            columns: vec![
                col("kaspa_txid", "BYTEA", "Kaspa txid"),
                col("recipient", "BYTEA", "L2 recipient"),
                col("amount_sompi", "INT8", "Sompi amount"),
                col("accepted_chain_block_hash", "BYTEA", "Kaspa chain block hash"),
                col("accepted_at", "TIMESTAMPTZ", "When accepted on Kaspa"),
                col("promote_after", "TIMESTAMPTZ", "Eligible-for-promotion timestamp"),
            ],
            examples: vec![],
        },
        TableInfo {
            name: "kaspa_provenance_meta",
            description: "Operational metadata for the Kaspa provenance sync (one row per chain). Mostly for ops; not interesting for app developers.",
            engines: pg(),
            columns: vec![
                col("chain_id", "INT8", "L2 chain id (38833 for Igra mainnet)"),
                col("txid_prefix", "BYTEA", "Igra txid prefix used for filtering Kaspa txs"),
                col("promotion_delay_secs", "INT8", "Pending → final promotion delay"),
            ],
            examples: vec![],
        },
        TableInfo {
            name: "kaspa_sync_state",
            description: "Operational state of the Kaspa provenance sync — checkpoint hash, last DAA score, last error/success timestamps.",
            engines: pg(),
            columns: vec![
                col("checkpoint_hash", "BYTEA", "Last successfully processed Kaspa chain block hash"),
                col("last_seen_sink", "BYTEA", "Last seen Kaspa sink (virtual chain tip)"),
                col("last_virtual_daa_score", "INT8", "DAA score of the last processed chain block"),
                col("tip_distance", "INT8", "Confirmation depth used for v2 calls / starting tip distance"),
                col("last_success_at", "TIMESTAMPTZ", "When sync last advanced"),
                col("last_error", "TEXT", "Last error message (NULL when healthy)"),
            ],
            examples: vec![],
        },
        TableInfo {
            name: "kaspa_provenance_gaps",
            description: "Operational table tracking gaps in the Kaspa provenance sync.",
            engines: pg(),
            columns: vec![
                col("range_start", "BYTEA", "Inclusive lower bound chain block hash of the gap"),
                col("range_end", "BYTEA", "Exclusive upper bound"),
            ],
            examples: vec![],
        },
    ]
}

/// Tips returned alongside the tables list — short, actionable hints about
/// API features developers commonly miss.
pub fn tips() -> Vec<&'static str> {
    vec![
        "Filter logs by event signature without manually hashing topic0: append `?signature=Transfer(address,address,uint256)` and SELECT FROM the event name as if it were a table — e.g., SELECT * FROM Transfer WHERE topic1 = decode('...','hex')",
        "Pagination: ORDER BY a stable key (block_num DESC, idx) and use LIMIT/OFFSET. Postgres caps each page at 10000 rows; ClickHouse caps at 50000 (use ?engine=clickhouse).",
        "Force the engine with ?engine=postgres or ?engine=clickhouse. Default routing picks one based on the query shape; CH is faster for analytical scans, PG for point lookups.",
        "uint256 columns (value, max_fee_per_gas, effective_gas_price) are stored as strings because they exceed 64 bits. In ClickHouse, cast with toUInt256OrZero() or compare as strings; in Postgres, cast with ::numeric.",
        "BYTEA columns: filter via decode('hex_no_prefix','hex'); display via encode(col,'hex'). In ClickHouse the same columns are hex-prefixed strings (e.g., '0xabc...') so LIKE '0xprefix%' works directly.",
        "internal_txs / call traces are not currently indexed (txs.calls is always NULL). Top-level value transfers are visible as txs.value > 0; internal transfers via contract calls are not.",
    ]
}

pub async fn handle_tables(
    State(_state): State<AppState>,
) -> Result<Json<TablesResponse>, ApiError> {
    Ok(Json(TablesResponse {
        ok: true,
        tables: tables_metadata(),
        tips: tips(),
    }))
}

/// Compact constructor — used inline above purely for readability.
const fn col(name: &'static str, ty: &'static str, description: &'static str) -> ColumnInfo {
    ColumnInfo { name, ty, description }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every table exposed by the `/query` validator must appear in the metadata,
    /// or callers can't discover it. The complement (metadata listing a table
    /// the validator forbids) is also a bug — covered by another test.
    #[test]
    fn tables_metadata_includes_all_allowed_tables() {
        let tables = tables_metadata();
        let names: Vec<&str> = tables.iter().map(|t| t.name).collect();

        for allowed in &[
            "blocks",
            "txs",
            "logs",
            "receipts",
            "l2_withdrawals",
            "kaspa_provenance_meta",
            "kaspa_sync_state",
            "kaspa_provenance_gaps",
            "kaspa_pending_l2_submissions",
            "kaspa_pending_entries",
            "kaspa_l2_submissions",
            "kaspa_entries",
        ] {
            assert!(
                names.contains(allowed),
                "table {allowed:?} missing from tables_metadata(); have {names:?}"
            );
        }
    }

    /// Inverse: nothing in the metadata should be a table the validator forbids,
    /// or we'd be advertising things that 403 on /query.
    #[test]
    fn tables_metadata_only_lists_allowed_tables() {
        // Mirror of validator::ALLOWED_TABLES; if that list grows this test
        // must be updated, which is the point.
        let allowed: std::collections::HashSet<&str> = [
            "blocks",
            "txs",
            "logs",
            "receipts",
            "l2_withdrawals",
            "kaspa_provenance_meta",
            "kaspa_sync_state",
            "kaspa_provenance_gaps",
            "kaspa_pending_l2_submissions",
            "kaspa_pending_entries",
            "kaspa_l2_submissions",
            "kaspa_entries",
        ]
        .into_iter()
        .collect();

        for table in tables_metadata() {
            assert!(
                allowed.contains(table.name),
                "metadata advertises {:?} but the /query validator does not allow it",
                table.name
            );
        }
    }

    #[test]
    fn every_table_has_at_least_one_column_and_a_description() {
        for table in tables_metadata() {
            assert!(
                !table.description.is_empty(),
                "table {:?} has empty description",
                table.name
            );
            assert!(
                !table.columns.is_empty(),
                "table {:?} has zero columns",
                table.name
            );
        }
    }

    #[test]
    fn engines_field_only_contains_known_engines() {
        for table in tables_metadata() {
            for engine in &table.engines {
                assert!(
                    *engine == "postgres" || *engine == "clickhouse",
                    "table {:?} has unknown engine {engine:?}",
                    table.name
                );
            }
            assert!(
                !table.engines.is_empty(),
                "table {:?} has no engines listed",
                table.name
            );
        }
    }

    /// Every column needs a name, a type, and a description. Empty strings would
    /// indicate metadata was scaffolded but never filled in.
    #[test]
    fn every_column_has_complete_metadata() {
        for table in tables_metadata() {
            for col in &table.columns {
                assert!(
                    !col.name.is_empty(),
                    "table {:?} has a column with empty name",
                    table.name
                );
                assert!(
                    !col.ty.is_empty(),
                    "{}.{} has empty type",
                    table.name,
                    col.name
                );
                assert!(
                    !col.description.is_empty(),
                    "{}.{} has empty description",
                    table.name,
                    col.name
                );
            }
        }
    }

    /// Tips are the user-facing answer to the most common discoverability issues
    /// the dev hit (signature helper, pagination, uint256 casting). Make sure
    /// those topics are at least mentioned.
    #[test]
    fn tips_cover_the_known_pain_points() {
        let body = tips().join("\n");
        assert!(
            body.contains("signature="),
            "tips should explain the ?signature= helper"
        );
        assert!(
            body.to_lowercase().contains("pagination") || body.contains("LIMIT") || body.contains("OFFSET"),
            "tips should explain pagination"
        );
        assert!(
            body.contains("uint256") || body.to_lowercase().contains("max_fee_per_gas") || body.contains("toUInt256"),
            "tips should explain uint256-as-string columns"
        );
        assert!(
            body.contains("internal") || body.contains("call traces"),
            "tips should warn that internal calls are not indexed yet"
        );
    }

    #[test]
    fn no_duplicate_table_names() {
        let names: Vec<&str> = tables_metadata().iter().map(|t| t.name).collect();
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(
            names.len(),
            unique.len(),
            "duplicate table name in metadata: {names:?}"
        );
    }

    #[test]
    fn no_duplicate_columns_within_a_table() {
        for table in tables_metadata() {
            let names: Vec<&str> = table.columns.iter().map(|c| c.name).collect();
            let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
            assert_eq!(
                names.len(),
                unique.len(),
                "table {:?} has duplicate column names: {names:?}",
                table.name
            );
        }
    }
}
