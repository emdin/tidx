use anyhow::{Result, anyhow};
use clickhouse::Row;
use serde::Serialize;

use crate::kaspa::writer::{FinalEntry, FinalL2Submission};
use crate::sync::ch_sink::hex_encode;

const L2_SCHEMA: &str = include_str!("../../db/clickhouse/kaspa_l2_submissions.sql");
const ENTRIES_SCHEMA: &str = include_str!("../../db/clickhouse/kaspa_entries.sql");
/// `ORDER BY` of L2_SCHEMA, as `system.tables.sorting_key` prints it.
const L2_SORTING_KEY: &str = "l2_tx_hash, kaspa_txid";

#[derive(Clone)]
pub struct KaspaClickHouseMirror {
    client: clickhouse::Client,
}

impl KaspaClickHouseMirror {
    pub fn new(url: &str, database: &str, user: Option<&str>, password: Option<&str>) -> Self {
        let mut client = clickhouse::Client::default()
            .with_url(url.trim_end_matches('/'))
            .with_database(database);
        if let Some(user) = user {
            client = client.with_user(user);
        }
        if let Some(password) = password {
            client = client.with_password(password);
        }
        Self { client }
    }

    pub async fn ensure_schema(&self) -> Result<()> {
        self.client
            .query(L2_SCHEMA)
            .execute()
            .await
            .map_err(|e| anyhow!("failed to create ClickHouse kaspa_l2_submissions: {e}"))?;
        self.client
            .query(ENTRIES_SCHEMA)
            .execute()
            .await
            .map_err(|e| anyhow!("failed to create ClickHouse kaspa_entries: {e}"))?;
        match self.rebuild_reasons().await {
            Ok(reasons) if reasons.is_empty() => {}
            Ok(reasons) => tracing::warn!(
                reasons = %reasons.join("; "),
                "ClickHouse Kaspa tables predate (l2_tx_hash, kaspa_txid) keys and 0x hex; \
                 run deploy/prod/ch-resync-kaspa.sh --rebuild"
            ),
            Err(e) => tracing::warn!(error = %e, "could not check the ClickHouse Kaspa tables"),
        }
        Ok(())
    }

    /// Why the Kaspa tables need `ch-resync-kaspa.sh --rebuild`, if they do:
    /// `CREATE TABLE IF NOT EXISTS` leaves a table from before the 2026-11
    /// change as it was, with kaspa_l2_submissions keyed on l2_tx_hash alone
    /// (one carrier per L2 tx) and hex without 0x. Rebuilding needs Postgres,
    /// so startup only warns.
    pub async fn rebuild_reasons(&self) -> Result<Vec<String>> {
        let mut reasons = Vec::new();
        let key = self
            .client
            .query(
                "SELECT sorting_key FROM system.tables \
                 WHERE database = currentDatabase() AND name = 'kaspa_l2_submissions'",
            )
            .fetch_optional::<String>()
            .await?;
        if let Some(key) = key.filter(|k| k != L2_SORTING_KEY) {
            reasons.push(format!("kaspa_l2_submissions is keyed on ({key})"));
        }
        for (table, bare) in [
            (
                "kaspa_l2_submissions",
                "NOT startsWith(l2_tx_hash, '0x') OR NOT startsWith(kaspa_txid, '0x')",
            ),
            (
                "kaspa_entries",
                "NOT startsWith(kaspa_txid, '0x') OR NOT startsWith(recipient, '0x')",
            ),
        ] {
            let found = self
                .client
                .query(&format!("SELECT 1 FROM {table} WHERE {bare} LIMIT 1"))
                .fetch_optional::<u8>()
                .await?;
            if found.is_some() {
                reasons.push(format!("{table} has hex without 0x"));
            }
        }
        Ok(reasons)
    }

    pub async fn write_l2_submissions(&self, rows: &[FinalL2Submission]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut insert = self
            .client
            .insert::<ChKaspaL2Submission>("kaspa_l2_submissions")
            .await?;
        for row in rows {
            insert.write(&ChKaspaL2Submission::from(row)).await?;
        }
        insert.end().await?;
        Ok(())
    }

    pub async fn write_entries(&self, rows: &[FinalEntry]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut insert = self.client.insert::<ChKaspaEntry>("kaspa_entries").await?;
        for row in rows {
            insert.write(&ChKaspaEntry::from(row)).await?;
        }
        insert.end().await?;
        Ok(())
    }
}

#[derive(Row, Serialize)]
struct ChKaspaL2Submission {
    l2_tx_hash: String,
    kaspa_txid: String,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    created_at: chrono::DateTime<chrono::Utc>,
}

impl From<&FinalL2Submission> for ChKaspaL2Submission {
    fn from(value: &FinalL2Submission) -> Self {
        Self {
            l2_tx_hash: hex_encode(&value.l2_tx_hash),
            kaspa_txid: hex_encode(&value.kaspa_txid),
            created_at: value.created_at,
        }
    }
}

#[derive(Row, Serialize)]
struct ChKaspaEntry {
    kaspa_txid: String,
    recipient: String,
    amount_sompi: u64,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    created_at: chrono::DateTime<chrono::Utc>,
}

impl From<&FinalEntry> for ChKaspaEntry {
    fn from(value: &FinalEntry) -> Self {
        Self {
            kaspa_txid: hex_encode(&value.kaspa_txid),
            recipient: hex_encode(&value.recipient),
            amount_sompi: value.amount_sompi,
            created_at: value.created_at,
        }
    }
}
