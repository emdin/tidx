//! ClickHouse direct-write sink.
//!
//! Writes blocks, transactions, logs, and receipts directly to ClickHouse
//! via the official `clickhouse` crate using RowBinary format with LZ4 compression.

use anyhow::{Result, anyhow};
use clickhouse::Row;
use serde::Serialize;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

use crate::metrics;
use crate::types::{BlockRow, L2WithdrawalRow, LogRow, ReceiptRow, TxRow};

/// Schema SQL files embedded at compile time.
const BLOCKS_SCHEMA: &str = include_str!("../../db/clickhouse/blocks.sql");
const TXS_SCHEMA: &str = include_str!("../../db/clickhouse/txs.sql");
const LOGS_SCHEMA: &str = include_str!("../../db/clickhouse/logs.sql");
const RECEIPTS_SCHEMA: &str = include_str!("../../db/clickhouse/receipts.sql");
const L2_WITHDRAWALS_SCHEMA: &str = include_str!("../../db/clickhouse/l2_withdrawals.sql");

/// Idempotent post-create ALTERs for `txs`. Each statement is `ADD COLUMN IF
/// NOT EXISTS` (or `ADD INDEX IF NOT EXISTS`), followed by `MATERIALIZE COLUMN`
/// for any column with a `DEFAULT` expression — without `MATERIALIZE`, CH
/// recomputes the DEFAULT on every read for parts that pre-date the ADD,
/// which makes UInt256 mirror queries scan the source String for ~1.3M rows
/// every time.
///
/// `MATERIALIZE COLUMN` is a CH mutation (async, non-blocking). It's
/// idempotent: once a part has the column materialized, subsequent runs are
/// no-ops (`parts_to_do=0`).
const TXS_COLUMN_ALTERS: &[&str] = &[
    // Phase 2: 4-byte ABI selector denormalized off `input`.
    "ALTER TABLE txs ADD COLUMN IF NOT EXISTS selector String DEFAULT '' AFTER input",
    "ALTER TABLE txs ADD INDEX IF NOT EXISTS idx_selector selector TYPE bloom_filter GRANULARITY 1",
    // Phase 3: UInt256 mirrors of wei-valued string columns.
    "ALTER TABLE txs ADD COLUMN IF NOT EXISTS value_u256 UInt256 DEFAULT toUInt256OrZero(value) AFTER value",
    "ALTER TABLE txs ADD COLUMN IF NOT EXISTS max_fee_per_gas_u256 UInt256 DEFAULT toUInt256OrZero(max_fee_per_gas) AFTER max_fee_per_gas",
    "ALTER TABLE txs ADD COLUMN IF NOT EXISTS max_priority_fee_per_gas_u256 UInt256 DEFAULT toUInt256OrZero(max_priority_fee_per_gas) AFTER max_priority_fee_per_gas",
    // Materialize so reads don't pay the toUInt256OrZero / coalesce cost
    // every time on parts that pre-date the ADD COLUMN.
    "ALTER TABLE txs MATERIALIZE COLUMN selector",
    "ALTER TABLE txs MATERIALIZE COLUMN value_u256",
    "ALTER TABLE txs MATERIALIZE COLUMN max_fee_per_gas_u256",
    "ALTER TABLE txs MATERIALIZE COLUMN max_priority_fee_per_gas_u256",
];

/// Idempotent post-create ALTERs for `logs`. `from` doesn't have a DEFAULT
/// expression (populated by the writer / backfill), so no MATERIALIZE needed.
const LOGS_COLUMN_ALTERS: &[&str] = &[
    "ALTER TABLE logs ADD COLUMN IF NOT EXISTS `from` Nullable(String) AFTER data",
    "ALTER TABLE logs ADD INDEX IF NOT EXISTS idx_from `from` TYPE bloom_filter GRANULARITY 1",
];

/// Idempotent post-create ALTERs for `receipts`.
const RECEIPTS_COLUMN_ALTERS: &[&str] = &[
    "ALTER TABLE receipts ADD COLUMN IF NOT EXISTS effective_gas_price_u256 Nullable(UInt256) DEFAULT toUInt256OrZero(effective_gas_price) AFTER effective_gas_price",
    "ALTER TABLE receipts MATERIALIZE COLUMN effective_gas_price_u256",
];

/// Max rows per ClickHouse INSERT to avoid unbounded memory growth during backfills.
const CH_INSERT_CHUNK_SIZE: usize = 10_000;

/// Max retry attempts for transient ClickHouse write failures.
const CH_MAX_RETRIES: u32 = 3;

/// Timeout for sending each chunk of row data to ClickHouse.
const CH_SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeout for waiting for ClickHouse to acknowledge the INSERT.
const CH_END_TIMEOUT: Duration = Duration::from_secs(120);

/// Direct-write ClickHouse sink using RowBinary format with LZ4 compression.
#[derive(Clone)]
pub struct ClickHouseSink {
    client: clickhouse::Client,
    /// Client without database context, used for `CREATE DATABASE` DDL.
    base_client: clickhouse::Client,
    database: String,
}

impl ClickHouseSink {
    /// Create a new ClickHouse sink.
    ///
    /// The database name is validated to prevent SQL injection in DDL statements
    /// that interpolate it (e.g., `CREATE DATABASE IF NOT EXISTS {database}`).
    ///
    /// Optional `user` and `password` enable HTTP basic auth for secured instances.
    pub fn new(
        url: &str,
        database: &str,
        user: Option<&str>,
        password: Option<&str>,
    ) -> Result<Self> {
        if !is_valid_identifier(database) {
            return Err(anyhow!(
                "Invalid ClickHouse database name '{database}': must be alphanumeric/underscore, \
                 start with a letter or underscore, and be 1-64 chars"
            ));
        }

        let url = url.trim_end_matches('/');
        let mut base_client = clickhouse::Client::default().with_url(url);
        if let Some(user) = user {
            base_client = base_client.with_user(user);
        }
        if let Some(password) = password {
            base_client = base_client.with_password(password);
        }
        let client = base_client.clone().with_database(database);

        Ok(Self {
            client,
            base_client,
            database: database.to_string(),
        })
    }

    /// Create database and tables if they don't exist.
    pub async fn ensure_schema(&self) -> Result<()> {
        self.base_client
            .query(&format!("CREATE DATABASE IF NOT EXISTS {}", self.database))
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to create ClickHouse database: {e}"))?;

        for (name, ddl) in [
            ("blocks", BLOCKS_SCHEMA),
            ("txs", TXS_SCHEMA),
            ("logs", LOGS_SCHEMA),
            ("receipts", RECEIPTS_SCHEMA),
            ("l2_withdrawals", L2_WITHDRAWALS_SCHEMA),
        ] {
            self.client
                .query(ddl)
                .execute()
                .await
                .map_err(|e| anyhow!("Failed to create ClickHouse table {name}: {e}"))?;
            debug!(table = name, database = %self.database, "ClickHouse table ready");
        }

        self.ensure_blocks_columns().await?;
        self.ensure_txs_columns().await?;
        self.ensure_logs_columns().await?;
        self.ensure_receipts_columns().await?;

        info!(database = %self.database, "ClickHouse schema ready");
        Ok(())
    }

    async fn ensure_txs_columns(&self) -> Result<()> {
        for ddl in TXS_COLUMN_ALTERS {
            self.client
                .query(ddl)
                .execute()
                .await
                .map_err(|e| anyhow!("Failed to alter ClickHouse txs table: {e}"))?;
        }
        Ok(())
    }

    async fn ensure_logs_columns(&self) -> Result<()> {
        for ddl in LOGS_COLUMN_ALTERS {
            self.client
                .query(ddl)
                .execute()
                .await
                .map_err(|e| anyhow!("Failed to alter ClickHouse logs table: {e}"))?;
        }
        Ok(())
    }

    async fn ensure_receipts_columns(&self) -> Result<()> {
        for ddl in RECEIPTS_COLUMN_ALTERS {
            self.client
                .query(ddl)
                .execute()
                .await
                .map_err(|e| anyhow!("Failed to alter ClickHouse receipts table: {e}"))?;
        }
        Ok(())
    }

    async fn ensure_blocks_columns(&self) -> Result<()> {
        for ddl in [
            "ALTER TABLE blocks ADD COLUMN IF NOT EXISTS real_timestamp Nullable(DateTime64(3, 'UTC')) AFTER timestamp_ms",
            "ALTER TABLE blocks ADD COLUMN IF NOT EXISTS real_timestamp_ms Nullable(Int64) AFTER real_timestamp",
            "ALTER TABLE blocks ADD COLUMN IF NOT EXISTS timestamp_drift_secs Nullable(Int32) AFTER real_timestamp_ms",
            "ALTER TABLE blocks ADD COLUMN IF NOT EXISTS l1_block_count Nullable(Int16) AFTER timestamp_drift_secs",
            "ALTER TABLE blocks ADD COLUMN IF NOT EXISTS l1_last_daa_score Nullable(Int64) AFTER l1_block_count",
            "ALTER TABLE blocks ADD COLUMN IF NOT EXISTS parent_beacon_block_root Nullable(String) AFTER l1_last_daa_score",
        ] {
            self.client
                .query(ddl)
                .execute()
                .await
                .map_err(|e| anyhow!("Failed to alter ClickHouse blocks table: {e}"))?;
        }
        Ok(())
    }

    pub fn name(&self) -> &'static str {
        "clickhouse"
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    pub async fn write_blocks(&self, blocks: &[BlockRow]) -> Result<()> {
        if blocks.is_empty() {
            return Ok(());
        }
        let start = Instant::now();
        self.insert_chunked("blocks", blocks, ChBlockWire::from_row)
            .await?;
        metrics::record_sink_write_duration(self.name(), "blocks", start.elapsed());
        metrics::record_sink_write_rows(self.name(), "blocks", blocks.len() as u64);
        metrics::update_sink_block_rate(self.name(), blocks.len() as u64);
        metrics::increment_sink_row_count(self.name(), "blocks", blocks.len() as u64);
        if let Some(max) = blocks.iter().map(|b| b.num).max() {
            metrics::update_sink_watermark(self.name(), "blocks", max);
        }
        Ok(())
    }

    pub async fn write_txs(&self, txs: &[TxRow]) -> Result<()> {
        if txs.is_empty() {
            return Ok(());
        }
        let start = Instant::now();
        self.insert_chunked("txs", txs, ChTxWire::from_row).await?;
        metrics::record_sink_write_duration(self.name(), "txs", start.elapsed());
        metrics::record_sink_write_rows(self.name(), "txs", txs.len() as u64);
        metrics::increment_sink_row_count(self.name(), "txs", txs.len() as u64);
        if let Some(max) = txs.iter().map(|t| t.block_num).max() {
            metrics::update_sink_watermark(self.name(), "txs", max);
        }
        Ok(())
    }

    pub async fn write_logs(&self, logs: &[LogRow]) -> Result<()> {
        if logs.is_empty() {
            return Ok(());
        }
        let start = Instant::now();
        self.insert_chunked("logs", logs, ChLogWire::from_row)
            .await?;
        metrics::record_sink_write_duration(self.name(), "logs", start.elapsed());
        metrics::record_sink_write_rows(self.name(), "logs", logs.len() as u64);
        metrics::increment_sink_row_count(self.name(), "logs", logs.len() as u64);
        if let Some(max) = logs.iter().map(|l| l.block_num).max() {
            metrics::update_sink_watermark(self.name(), "logs", max);
        }
        Ok(())
    }

    pub async fn write_receipts(&self, receipts: &[ReceiptRow]) -> Result<()> {
        if receipts.is_empty() {
            return Ok(());
        }
        let start = Instant::now();
        self.insert_chunked("receipts", receipts, ChReceiptWire::from_row)
            .await?;
        metrics::record_sink_write_duration(self.name(), "receipts", start.elapsed());
        metrics::record_sink_write_rows(self.name(), "receipts", receipts.len() as u64);
        metrics::increment_sink_row_count(self.name(), "receipts", receipts.len() as u64);
        if let Some(max) = receipts.iter().map(|r| r.block_num).max() {
            metrics::update_sink_watermark(self.name(), "receipts", max);
        }
        Ok(())
    }

    pub async fn write_l2_withdrawals(&self, withdrawals: &[L2WithdrawalRow]) -> Result<()> {
        if withdrawals.is_empty() {
            return Ok(());
        }
        let start = Instant::now();
        self.insert_chunked("l2_withdrawals", withdrawals, ChL2WithdrawalWire::from_row)
            .await?;
        metrics::record_sink_write_duration(self.name(), "l2_withdrawals", start.elapsed());
        metrics::record_sink_write_rows(self.name(), "l2_withdrawals", withdrawals.len() as u64);
        metrics::increment_sink_row_count(self.name(), "l2_withdrawals", withdrawals.len() as u64);
        if let Some(max) = withdrawals.iter().map(|w| w.block_num).max() {
            metrics::update_sink_watermark(self.name(), "l2_withdrawals", max);
        }
        Ok(())
    }

    /// Query the highest block number in ClickHouse, or None if empty.
    pub async fn max_block_num(&self) -> Result<Option<i64>> {
        let count: u64 = self
            .client
            .query("SELECT count() FROM blocks")
            .fetch_one()
            .await
            .map_err(|e| anyhow!("ClickHouse query failed: {e}"))?;
        if count == 0 {
            return Ok(None);
        }
        let max: i64 = self
            .client
            .query("SELECT max(num) FROM blocks")
            .fetch_one()
            .await
            .map_err(|e| anyhow!("ClickHouse query failed: {e}"))?;
        Ok(Some(max))
    }

    /// Query the highest block number for a specific table.
    /// Uses "num" for blocks table, "block_num" for others.
    /// Returns None if the table is empty.
    pub async fn max_block_in_table(&self, table: &str) -> Result<Option<i64>> {
        let table = validate_table_name(table)?;
        let col = if table == "blocks" {
            "num"
        } else {
            "block_num"
        };
        let count: u64 = self
            .client
            .query(&format!("SELECT count() FROM {table}"))
            .fetch_one()
            .await
            .map_err(|e| anyhow!("ClickHouse query failed: {e}"))?;
        if count == 0 {
            return Ok(None);
        }
        let max: i64 = self
            .client
            .query(&format!("SELECT max({col}) FROM {table}"))
            .fetch_one()
            .await
            .map_err(|e| anyhow!("ClickHouse query failed: {e}"))?;
        Ok(Some(max))
    }

    /// Query the row count for a specific table.
    pub async fn row_count(&self, table: &str) -> Result<u64> {
        let table = validate_table_name(table)?;
        self.client
            .query(&format!("SELECT count() FROM {table}"))
            .fetch_one()
            .await
            .map_err(|e| anyhow!("ClickHouse query failed: {e}"))
    }

    /// Delete all data from a given block number onwards (reorg support).
    pub async fn delete_from(&self, block_num: u64) -> Result<()> {
        let tables = ["logs", "receipts", "txs", "l2_withdrawals", "blocks"];
        let block_col = |t: &str| if t == "blocks" { "num" } else { "block_num" };

        for table in &tables {
            let sql = format!(
                "ALTER TABLE {} DELETE WHERE {} >= {}",
                table,
                block_col(table),
                block_num
            );
            self.client
                .query(&sql)
                .with_option("mutations_sync", "1")
                .execute()
                .await
                .map_err(|e| {
                    error!(table = *table, error = %e, "ClickHouse delete failed");
                    anyhow!("ClickHouse delete from {table} failed: {e}")
                })?;
        }

        debug!(from_block = block_num, "ClickHouse reorg delete complete");
        Ok(())
    }

    /// Chunk source rows, convert each chunk to wire format, and insert with retry logic.
    /// This avoids allocating the full wire-format vec upfront, bounding peak memory
    /// to `CH_INSERT_CHUNK_SIZE` wire structs at a time.
    async fn insert_chunked<S, W, F>(&self, table: &str, rows: &[S], convert: F) -> Result<()>
    where
        W: Serialize + for<'a> Row<Value<'a> = W>,
        F: Fn(&S) -> W,
    {
        for chunk in rows.chunks(CH_INSERT_CHUNK_SIZE) {
            let wire: Vec<W> = chunk.iter().map(&convert).collect();
            let mut last_error = None;
            for attempt in 0..CH_MAX_RETRIES {
                if attempt > 0 {
                    let backoff = Duration::from_millis(100 << attempt);
                    warn!(table, attempt, "ClickHouse insert retry after {backoff:?}");
                    tokio::time::sleep(backoff).await;
                }
                match self.try_insert(table, &wire).await {
                    Ok(()) => {
                        last_error = None;
                        break;
                    }
                    Err(e) => {
                        last_error = Some(e);
                    }
                }
            }
            if let Some(e) = last_error {
                return Err(anyhow!(
                    "ClickHouse insert into {table} failed after {CH_MAX_RETRIES} attempts: {e}"
                ));
            }
        }
        Ok(())
    }

    async fn try_insert<T>(&self, table: &str, rows: &[T]) -> Result<()>
    where
        T: Serialize + for<'a> Row<Value<'a> = T>,
    {
        let mut insert = self
            .client
            .insert::<T>(table)
            .await?
            .with_timeouts(Some(CH_SEND_TIMEOUT), Some(CH_END_TIMEOUT));
        for row in rows {
            insert.write(row).await?;
        }
        insert.end().await?;
        Ok(())
    }
}

// ── ClickHouse wire-format structs ────────────────────────────────────────
//
// These derive `clickhouse::Row` for RowBinary serialization and `serde::Serialize`
// for the Row encoding. DateTime64(3) columns use the chrono serde adapter.

#[derive(Row, Serialize)]
struct ChBlockWire {
    num: i64,
    hash: String,
    parent_hash: String,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    timestamp: chrono::DateTime<chrono::Utc>,
    timestamp_ms: i64,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis::option")]
    real_timestamp: Option<chrono::DateTime<chrono::Utc>>,
    real_timestamp_ms: Option<i64>,
    timestamp_drift_secs: Option<i32>,
    l1_block_count: Option<i16>,
    l1_last_daa_score: Option<i64>,
    parent_beacon_block_root: Option<String>,
    gas_limit: i64,
    gas_used: i64,
    miner: String,
    extra_data: Option<String>,
}

impl ChBlockWire {
    fn from_row(b: &BlockRow) -> Self {
        Self {
            num: b.num,
            hash: hex_encode(&b.hash),
            parent_hash: hex_encode(&b.parent_hash),
            timestamp: b.timestamp,
            timestamp_ms: b.timestamp_ms,
            real_timestamp: b.real_timestamp,
            real_timestamp_ms: b.real_timestamp_ms,
            timestamp_drift_secs: b.timestamp_drift_secs,
            l1_block_count: b.l1_block_count,
            l1_last_daa_score: b.l1_last_daa_score,
            parent_beacon_block_root: b.parent_beacon_block_root.as_ref().map(|v| hex_encode(v)),
            gas_limit: b.gas_limit,
            gas_used: b.gas_used,
            miner: hex_encode(&b.miner),
            extra_data: b.extra_data.as_ref().map(|v| hex_encode(v)),
        }
    }
}

#[derive(Row, Serialize)]
struct ChTxWire {
    block_num: i64,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    block_timestamp: chrono::DateTime<chrono::Utc>,
    idx: i32,
    hash: String,
    #[serde(rename = "type")]
    tx_type: i16,
    from: String,
    to: Option<String>,
    value: String,
    input: String,
    /// Empty string when input is shorter than 4 bytes — matches the CH column
    /// `String DEFAULT ''` so the bloom_filter index covers the empty case.
    selector: String,
    gas_limit: i64,
    max_fee_per_gas: String,
    max_priority_fee_per_gas: String,
    gas_used: Option<i64>,
    nonce_key: String,
    nonce: i64,
    fee_token: Option<String>,
    fee_payer: Option<String>,
    calls: Option<String>,
    call_count: i16,
    valid_before: Option<i64>,
    valid_after: Option<i64>,
    signature_type: Option<i16>,
}

impl ChTxWire {
    fn from_row(tx: &TxRow) -> Self {
        Self {
            block_num: tx.block_num,
            block_timestamp: tx.block_timestamp,
            idx: tx.idx,
            hash: hex_encode(&tx.hash),
            tx_type: tx.tx_type,
            from: hex_encode(&tx.from),
            to: tx.to.as_ref().map(|v| hex_encode(v)),
            value: tx.value.clone(),
            input: hex_encode(&tx.input),
            selector: tx
                .selector
                .as_ref()
                .map(|v| hex_encode(v))
                .unwrap_or_default(),
            gas_limit: tx.gas_limit,
            max_fee_per_gas: tx.max_fee_per_gas.clone(),
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas.clone(),
            gas_used: tx.gas_used,
            nonce_key: hex_encode(&tx.nonce_key),
            nonce: tx.nonce,
            fee_token: tx.fee_token.as_ref().map(|v| hex_encode(v)),
            fee_payer: tx.fee_payer.as_ref().map(|v| hex_encode(v)),
            calls: tx.calls.as_ref().map(|v| v.to_string()),
            call_count: tx.call_count,
            valid_before: tx.valid_before,
            valid_after: tx.valid_after,
            signature_type: tx.signature_type,
        }
    }
}

#[derive(Row, Serialize)]
struct ChLogWire {
    block_num: i64,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    block_timestamp: chrono::DateTime<chrono::Utc>,
    log_idx: i32,
    tx_idx: i32,
    tx_hash: String,
    address: String,
    selector: String,
    topic0: Option<String>,
    topic1: Option<String>,
    topic2: Option<String>,
    topic3: Option<String>,
    data: String,
    /// Denormalized parent-tx sender. Nullable so old rows (pre-migration)
    /// stay queryable while backfill is in flight.
    from: Option<String>,
}

impl ChLogWire {
    fn from_row(log: &LogRow) -> Self {
        Self {
            block_num: log.block_num,
            block_timestamp: log.block_timestamp,
            log_idx: log.log_idx,
            tx_idx: log.tx_idx,
            tx_hash: hex_encode(&log.tx_hash),
            address: hex_encode(&log.address),
            selector: log
                .selector
                .as_ref()
                .map(|v| hex_encode(v))
                .unwrap_or_default(),
            topic0: log.topic0.as_ref().map(|v| hex_encode(v)),
            topic1: log.topic1.as_ref().map(|v| hex_encode(v)),
            topic2: log.topic2.as_ref().map(|v| hex_encode(v)),
            topic3: log.topic3.as_ref().map(|v| hex_encode(v)),
            data: hex_encode(&log.data),
            from: log.from.as_ref().map(|v| hex_encode(v)),
        }
    }
}

#[derive(Row, Serialize)]
struct ChReceiptWire {
    block_num: i64,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    block_timestamp: chrono::DateTime<chrono::Utc>,
    tx_idx: i32,
    tx_hash: String,
    from: String,
    to: Option<String>,
    contract_address: Option<String>,
    gas_used: i64,
    cumulative_gas_used: i64,
    effective_gas_price: Option<String>,
    status: Option<i16>,
    fee_payer: Option<String>,
}

impl ChReceiptWire {
    fn from_row(r: &ReceiptRow) -> Self {
        Self {
            block_num: r.block_num,
            block_timestamp: r.block_timestamp,
            tx_idx: r.tx_idx,
            tx_hash: hex_encode(&r.tx_hash),
            from: hex_encode(&r.from),
            to: r.to.as_ref().map(|v| hex_encode(v)),
            contract_address: r.contract_address.as_ref().map(|v| hex_encode(v)),
            gas_used: r.gas_used,
            cumulative_gas_used: r.cumulative_gas_used,
            effective_gas_price: r.effective_gas_price.clone(),
            status: r.status,
            fee_payer: r.fee_payer.as_ref().map(|v| hex_encode(v)),
        }
    }
}

#[derive(Row, Serialize)]
struct ChL2WithdrawalWire {
    block_num: i64,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    block_timestamp: chrono::DateTime<chrono::Utc>,
    idx: i32,
    withdrawal_index: String,
    index_le: String,
    validator_index: String,
    address: String,
    amount_gwei: i64,
    amount_sompi: i64,
}

impl ChL2WithdrawalWire {
    fn from_row(w: &L2WithdrawalRow) -> Self {
        Self {
            block_num: w.block_num,
            block_timestamp: w.block_timestamp,
            idx: w.idx,
            withdrawal_index: w.withdrawal_index.clone(),
            index_le: hex_encode(&w.index_le),
            validator_index: w.validator_index.clone(),
            address: hex_encode(&w.address),
            amount_gwei: w.amount_gwei,
            amount_sompi: w.amount_sompi,
        }
    }
}

/// Hex-encode bytes with 0x prefix.
fn hex_encode(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// Known table names that are safe to interpolate into SQL.
const KNOWN_TABLES: &[&str] = &["blocks", "txs", "logs", "receipts", "l2_withdrawals"];

/// Validate that a table name is one of the known tables.
/// Returns the validated name or an error for unknown tables.
fn validate_table_name(table: &str) -> Result<&str> {
    KNOWN_TABLES
        .iter()
        .find(|&&t| t == table)
        .copied()
        .ok_or_else(|| anyhow!("Unknown ClickHouse table: {table}"))
}

/// Validate that a string is a safe SQL identifier (for table/database names
/// interpolated into DDL/queries). Allows `[a-zA-Z_][a-zA-Z0-9_]{0,63}`.
fn is_valid_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "0xdeadbeef");
        assert_eq!(hex_encode(&[]), "0x");
    }

    #[test]
    fn test_wire_struct_serialization() {
        use chrono::TimeZone;
        let dt = chrono::Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap();

        let block = crate::types::BlockRow {
            num: 42,
            hash: vec![0xab; 32],
            parent_hash: vec![0xcd; 32],
            timestamp: dt,
            timestamp_ms: 1705320000000,
            real_timestamp: Some(dt - chrono::Duration::seconds(150)),
            real_timestamp_ms: Some(1705319850000),
            timestamp_drift_secs: Some(150),
            l1_block_count: Some(10),
            l1_last_daa_score: Some(123456),
            parent_beacon_block_root: Some(vec![0x01; 32]),
            gas_limit: 30_000_000,
            gas_used: 15_000_000,
            miner: vec![0xee; 20],
            extra_data: None,
        };

        let wire = ChBlockWire::from_row(&block);
        // Verify field values via the struct fields directly
        assert_eq!(wire.num, 42);
        assert_eq!(wire.hash, format!("0x{}", "ab".repeat(32)));
        assert_eq!(wire.miner, format!("0x{}", "ee".repeat(20)));
        assert_eq!(wire.timestamp, dt);
        assert!(wire.extra_data.is_none());
    }

    #[test]
    fn test_wire_struct_tx_type_rename() {
        let tx = crate::types::TxRow {
            tx_type: 2,
            ..Default::default()
        };

        let wire = ChTxWire::from_row(&tx);
        // Verify via serde JSON that the rename applies
        let json = serde_json::to_string(&wire).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], 2);
        assert!(parsed.get("tx_type").is_none());
    }

    #[test]
    fn test_valid_identifier() {
        assert!(is_valid_identifier("tidx_4217"));
        assert!(is_valid_identifier("blocks"));
        assert!(is_valid_identifier("_private"));
        assert!(is_valid_identifier("A"));

        assert!(!is_valid_identifier(""));
        assert!(!is_valid_identifier("123abc"));
        assert!(!is_valid_identifier("my-db"));
        assert!(!is_valid_identifier("db; DROP TABLE x"));
        assert!(!is_valid_identifier("db name"));
        assert!(!is_valid_identifier(&"a".repeat(65)));
    }

    #[test]
    fn test_new_rejects_bad_database_name() {
        assert!(ClickHouseSink::new("http://localhost:8123", "tidx_4217", None, None).is_ok());
        assert!(
            ClickHouseSink::new(
                "http://localhost:8123",
                "foo; DROP TABLE blocks",
                None,
                None
            )
            .is_err()
        );
        assert!(ClickHouseSink::new("http://localhost:8123", "123bad", None, None).is_err());
        assert!(ClickHouseSink::new("http://localhost:8123", "", None, None).is_err());
    }

    /// Every column declared with `DEFAULT` (i.e. computed-from-other-columns)
    /// MUST be matched by a `MATERIALIZE COLUMN` in the same alter list, or CH
    /// recomputes the DEFAULT on every read for parts that pre-date the ADD.
    /// This test enforces that invariant for the txs alter list.
    #[test]
    fn txs_alter_list_materializes_every_default_column() {
        let alters = TXS_COLUMN_ALTERS;
        for col in &[
            "selector",
            "value_u256",
            "max_fee_per_gas_u256",
            "max_priority_fee_per_gas_u256",
        ] {
            let has_add = alters
                .iter()
                .any(|s| s.contains(&format!("ADD COLUMN IF NOT EXISTS {col} ")));
            let has_materialize = alters
                .iter()
                .any(|s| s.contains(&format!("MATERIALIZE COLUMN {col}")));
            assert!(has_add, "txs.{col}: ADD COLUMN missing");
            assert!(
                has_materialize,
                "txs.{col}: MATERIALIZE COLUMN missing — old parts will recompute DEFAULT on every read"
            );
        }
    }

    #[test]
    fn receipts_alter_list_materializes_every_default_column() {
        let alters = RECEIPTS_COLUMN_ALTERS;
        for col in &["effective_gas_price_u256"] {
            assert!(
                alters
                    .iter()
                    .any(|s| s.contains(&format!("ADD COLUMN IF NOT EXISTS {col} "))),
                "receipts.{col}: ADD COLUMN missing"
            );
            assert!(
                alters
                    .iter()
                    .any(|s| s.contains(&format!("MATERIALIZE COLUMN {col}"))),
                "receipts.{col}: MATERIALIZE COLUMN missing"
            );
        }
    }

    /// `MATERIALIZE COLUMN` must come AFTER its corresponding `ADD COLUMN` in
    /// the alter list — CH parses left-to-right and a MATERIALIZE on a
    /// not-yet-existing column would fail.
    #[test]
    fn txs_alter_list_orders_add_before_materialize() {
        for col in &[
            "selector",
            "value_u256",
            "max_fee_per_gas_u256",
            "max_priority_fee_per_gas_u256",
        ] {
            let add_pos = TXS_COLUMN_ALTERS
                .iter()
                .position(|s| s.contains(&format!("ADD COLUMN IF NOT EXISTS {col} ")))
                .unwrap_or_else(|| panic!("ADD COLUMN for txs.{col} missing"));
            let mat_pos = TXS_COLUMN_ALTERS
                .iter()
                .position(|s| s.contains(&format!("MATERIALIZE COLUMN {col}")))
                .unwrap_or_else(|| panic!("MATERIALIZE COLUMN for txs.{col} missing"));
            assert!(
                add_pos < mat_pos,
                "txs.{col}: MATERIALIZE COLUMN at position {mat_pos} comes before ADD COLUMN at {add_pos}"
            );
        }
    }

    /// `logs."from"` is populated by the writer (no DEFAULT expression), so
    /// MATERIALIZE COLUMN is unnecessary. Asserting the absence prevents a
    /// well-meaning future change from adding a no-op MATERIALIZE that just
    /// burns CPU at startup.
    #[test]
    fn logs_alter_list_has_no_materialize() {
        let mat = LOGS_COLUMN_ALTERS
            .iter()
            .filter(|s| s.contains("MATERIALIZE COLUMN"))
            .count();
        assert_eq!(
            mat, 0,
            "logs has no DEFAULT-computed columns; MATERIALIZE shouldn't be needed"
        );
    }
}
