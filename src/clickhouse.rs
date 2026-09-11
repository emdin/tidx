//! ClickHouse OLAP engine for analytical queries.
//!
//! Reads from tables populated by the direct-write ClickHouseSink.
//! Provides vectorized columnar execution for OLAP queries.
//!
//! Supports multiple ClickHouse instances per chain with failover:
//! queries go to the primary instance and automatically fail over
//! to secondary instances if the primary is unavailable.

use anyhow::{Result, anyhow};
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::{error, warn};

use crate::config::ClickHouseConfig;
use crate::query::{
    enforce_limit, extract_raw_column_predicates, normalize_hex_literals_clickhouse,
    validate_query_with_cap, EventSignature, HARD_LIMIT_CLICKHOUSE,
};

/// A single ClickHouse instance (connection + URL).
struct Instance {
    http_client: reqwest::Client,
    url: String,
    user: Option<String>,
    password: Option<String>,
}

/// ClickHouse engine for OLAP queries.
///
/// When multiple instances are configured, queries are sent to the active
/// instance (starting with the primary). On connection failure the engine
/// automatically tries the next instance in order.
pub struct ClickHouseEngine {
    instances: Vec<Instance>,
    /// Index of the currently active instance (0 = primary).
    active: AtomicUsize,
    /// Database name for this chain (e.g., "tidx_4217" for chain 4217)
    database: String,
}

impl ClickHouseEngine {
    /// Create a new ClickHouse engine for the given chain.
    /// The primary URL comes from `config.url`; additional failover URLs
    /// come from `config.failover_urls`.
    pub fn new(config: &ClickHouseConfig, chain_id: u64) -> Result<Self> {
        let database = config
            .database
            .clone()
            .unwrap_or_else(|| format!("tidx_{chain_id}"));

        let password = config.resolved_password()?;
        let mut instances = Vec::new();
        for url in config.all_urls() {
            instances.push(Self::make_instance(
                url,
                config.user.clone(),
                password.clone(),
            )?);
        }

        Ok(Self {
            instances,
            active: AtomicUsize::new(0),
            database,
        })
    }

    fn make_instance(
        url: &str,
        user: Option<String>,
        password: Option<String>,
    ) -> Result<Instance> {
        let http_client = reqwest::Client::builder()
            .pool_max_idle_per_host(4)
            .build()
            .map_err(|e| anyhow!("Failed to create HTTP client: {e}"))?;
        Ok(Instance {
            http_client,
            url: url.to_string(),
            user,
            password,
        })
    }

    /// Get the database name.
    pub fn database(&self) -> &str {
        &self.database
    }

    /// Prefix `sql` with the event CTEs derived from `signatures`, so that
    /// `FROM <EventName>` resolves. Pure; no I/O.
    fn wrap_signatures(sql: &str, signatures: &[&str]) -> Result<String> {
        if signatures.is_empty() {
            return Ok(sql.to_string());
        }
        let sigs: Vec<EventSignature> = signatures
            .iter()
            .map(|s| EventSignature::parse(s))
            .collect::<Result<_>>()?;

        let sql = sigs.iter().fold(sql.to_string(), |sql, sig| {
            sig.rewrite_filters_for_pushdown(&sig.normalize_table_references(&sql))
        });
        let pushdown = extract_raw_column_predicates(&sql);
        let ctes: Vec<String> = sigs
            .iter()
            .map(|sig| sig.to_cte_sql_clickhouse_with_pushdown(None, &pushdown))
            .collect();
        Ok(format!("WITH {} {sql}", ctes.join(", ")))
    }

    /// Everything the PUBLIC `/query` path must do to untrusted SQL before it
    /// touches ClickHouse, as a pure function so it is unit-testable without a
    /// live server: CTE-wrap, run the same allowlist validator Postgres uses,
    /// and enforce the row cap. Mirrors `service::execute_query_postgres`.
    ///
    /// This exists because the ClickHouse path previously ran NO validation —
    /// `engine=clickhouse` could read `system.tables`, and the server runs as
    /// `default` with `readonly=0`. Trusted internal callers that need DDL
    /// (`views.rs`) use `query` directly and are not subject to this.
    pub fn prepare_public_sql(sql: &str, signatures: &[&str], limit: i64) -> Result<String> {
        let sql = Self::wrap_signatures(sql, signatures)?;
        validate_query_with_cap(&sql, HARD_LIMIT_CLICKHOUSE)?;
        Ok(enforce_limit(&sql, limit, HARD_LIMIT_CLICKHOUSE))
    }

    /// Public `/query` entry point: validated + capped, then executed.
    pub async fn query_public(
        &self,
        sql: &str,
        signatures: &[&str],
        limit: i64,
    ) -> Result<QueryResult> {
        let sql = Self::prepare_public_sql(sql, signatures, limit)?;
        self.run(&sql).await
    }

    /// Trusted entry point (no validation, no cap) for internal callers such as
    /// `views.rs`, which issue DDL. NOT for user-supplied SQL — use
    /// `query_public`.
    pub async fn query(&self, sql: &str, signatures: &[&str]) -> Result<QueryResult> {
        let sql = Self::wrap_signatures(sql, signatures)?;
        self.run(&sql).await
    }

    /// Execute already-prepared SQL, failing over across instances on
    /// connection errors. ClickHouse query errors (syntax, missing table) are
    /// returned immediately. Hex-literal case folding lives here because it is
    /// an execution concern for every CH statement: columns are lowercase
    /// 0x-strings compared case-sensitively, so a checksummed literal would
    /// silently match nothing. It is a no-op on DDL.
    async fn run(&self, sql: &str) -> Result<QueryResult> {
        let sql = normalize_hex_literals_clickhouse(sql);

        let start = std::time::Instant::now();
        let n = self.instances.len();
        let starting = self.active.load(Ordering::Relaxed);

        for attempt in 0..n {
            let idx = (starting + attempt) % n;
            let inst = &self.instances[idx];

            match self.try_query(inst, &sql, start).await {
                Ok(result) => {
                    if attempt > 0 {
                        self.active.store(idx, Ordering::Relaxed);
                        warn!(
                            url = %inst.url,
                            database = %self.database,
                            "ClickHouse failed over to instance {}",
                            idx
                        );
                    }
                    return Ok(result);
                }
                Err(e) if is_connection_error(&e) && attempt + 1 < n => {
                    error!(
                        url = %inst.url,
                        error = %e,
                        database = %self.database,
                        "ClickHouse instance unreachable, trying next"
                    );
                }
                Err(e) => return Err(e),
            }
        }

        Err(anyhow!("All ClickHouse instances unreachable"))
    }

    async fn try_query(
        &self,
        inst: &Instance,
        sql: &str,
        start: std::time::Instant,
    ) -> Result<QueryResult> {
        let url = format!(
            "{}/?database={}&default_format=JSON",
            inst.url.trim_end_matches('/'),
            self.database
        );

        let mut req = inst.http_client.post(&url).body(sql.to_string());
        if let Some(ref user) = inst.user {
            req = req.header("X-ClickHouse-User", user);
        }
        if let Some(ref password) = inst.password {
            req = req.header("X-ClickHouse-Key", password);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| anyhow!("ClickHouse HTTP request failed: {e}"))?;

        if !resp.status().is_success() {
            let error_text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("ClickHouse query failed: {error_text}"));
        }

        let json_response = resp
            .text()
            .await
            .map_err(|e| anyhow!("Failed to read response: {e}"))?;

        if json_response.trim().is_empty() {
            return Ok(QueryResult {
                columns: vec![],
                rows: vec![],
                row_count: 0,
                engine: Some("clickhouse".to_string()),
                query_time_ms: Some(start.elapsed().as_secs_f64() * 1000.0),
            });
        }

        let parsed: serde_json::Value = serde_json::from_str(&json_response)
            .map_err(|e| anyhow!("Failed to parse ClickHouse JSON response: {e}"))?;

        let meta = parsed.get("meta").and_then(|m| m.as_array());
        let data = parsed.get("data").and_then(|d| d.as_array());

        let columns: Vec<String> = meta
            .map(|m| {
                m.iter()
                    .filter_map(|col| col.get("name").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let rows: Vec<Vec<serde_json::Value>> = data
            .map(|d| {
                d.iter()
                    .map(|row| {
                        columns
                            .iter()
                            .map(|col| row.get(col).cloned().unwrap_or(serde_json::Value::Null))
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default();

        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        let row_count = rows.len();

        Ok(QueryResult {
            columns,
            rows,
            row_count,
            engine: Some("clickhouse".to_string()),
            query_time_ms: Some(elapsed_ms),
        })
    }

    /// Return the URL of the currently active instance (for observability).
    pub fn active_url(&self) -> &str {
        let idx = self.active.load(Ordering::Relaxed);
        &self.instances[idx].url
    }

    /// Return the number of configured instances.
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }
}

/// Returns true for errors that indicate the ClickHouse instance is unreachable
/// (connection refused, timeout, DNS failure, etc.) — as opposed to query-level
/// errors that would happen on any instance.
fn is_connection_error(err: &anyhow::Error) -> bool {
    let msg = err.to_string();
    msg.contains("HTTP request failed")
        || msg.contains("connection refused")
        || msg.contains("Connection refused")
        || msg.contains("connect error")
        || msg.contains("dns error")
        || msg.contains("timed out")
        || msg.contains("hyper::Error")
}

/// Query result from ClickHouse.
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    pub engine: Option<String>,
    pub query_time_ms: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_connection_error() {
        let conn_err = anyhow!("ClickHouse HTTP request failed: connection refused");
        assert!(is_connection_error(&conn_err));

        let query_err =
            anyhow!("ClickHouse query failed: Code: 60. DB::Exception: Table logs doesn't exist");
        assert!(!is_connection_error(&query_err));
    }

    #[test]
    fn test_engine_single_instance() {
        let config = ClickHouseConfig {
            enabled: true,
            url: "http://clickhouse-1:8123".to_string(),
            failover_urls: vec![],
            database: None,
            ..Default::default()
        };

        let engine = ClickHouseEngine::new(&config, 4217).unwrap();
        assert_eq!(engine.instance_count(), 1);
        assert_eq!(engine.active_url(), "http://clickhouse-1:8123");
    }

    #[test]
    fn test_engine_multiple_instances() {
        let config = ClickHouseConfig {
            enabled: true,
            url: "http://clickhouse-1:8123".to_string(),
            failover_urls: vec!["http://clickhouse-2:8123".to_string()],
            database: None,
            ..Default::default()
        };

        let engine = ClickHouseEngine::new(&config, 4217).unwrap();
        assert_eq!(engine.instance_count(), 2);
        assert_eq!(engine.active_url(), "http://clickhouse-1:8123");
    }

    #[test]
    fn test_engine_database_override() {
        let config = ClickHouseConfig {
            enabled: true,
            url: "http://clickhouse-1:8123".to_string(),
            failover_urls: vec![],
            database: Some("custom_db".to_string()),
            ..Default::default()
        };

        let engine = ClickHouseEngine::new(&config, 4217).unwrap();
        assert_eq!(engine.database(), "custom_db");
    }

    #[test]
    fn test_engine_database_default() {
        let config = ClickHouseConfig {
            enabled: true,
            url: "http://clickhouse-1:8123".to_string(),
            failover_urls: vec![],
            database: None,
            ..Default::default()
        };

        let engine = ClickHouseEngine::new(&config, 4217).unwrap();
        assert_eq!(engine.database(), "tidx_4217");
    }

    // ---- prepare_public_sql: the public /query policy pipeline, no I/O ----
    //
    // The ClickHouse path previously ran NO validator: `engine=clickhouse`
    // could read `system.tables` (verified on prod 2026-09-11) while the same
    // query was rejected on Postgres. These pin the closed gap and the cap.

    use crate::query::HARD_LIMIT_CLICKHOUSE;
    const SIG: &str = "Transfer(address,address,uint256)";

    #[test]
    fn public_sql_rejects_non_allowlisted_table() {
        let err = ClickHouseEngine::prepare_public_sql(
            "SELECT name FROM system.tables",
            &[],
            100,
        )
        .expect_err("system.tables must be rejected on the CH public path");
        assert!(err.to_string().contains("not allowed"), "got: {err}");
    }

    #[test]
    fn public_sql_rejects_non_select() {
        assert!(ClickHouseEngine::prepare_public_sql("DROP TABLE logs", &[], 100).is_err());
        assert!(ClickHouseEngine::prepare_public_sql("SELECT 1; DROP TABLE logs", &[], 100).is_err());
    }

    #[test]
    fn public_sql_appends_caller_page_size_when_no_limit() {
        let sql = ClickHouseEngine::prepare_public_sql("SELECT num FROM blocks", &[], 250).unwrap();
        assert_eq!(sql, "SELECT num FROM blocks LIMIT 250");
    }

    /// Over the CH cap is rejected LOUDLY by the validator (not silently
    /// clamped — a quiet truncation is the class of bug the dev flagged), and
    /// the message names the ClickHouse cap, not Postgres's.
    #[test]
    fn public_sql_rejects_limit_over_ch_cap_loudly() {
        let err = ClickHouseEngine::prepare_public_sql(
            "SELECT num FROM blocks LIMIT 999999",
            &[],
            HARD_LIMIT_CLICKHOUSE,
        )
        .expect_err("over-cap LIMIT must be rejected");
        assert!(
            err.to_string().contains(&format!("exceeds maximum ({HARD_LIMIT_CLICKHOUSE})")),
            "must cite the CH cap; got: {err}"
        );
    }

    /// The regression guard for making the validator engine-aware: a LIMIT
    /// between Postgres's 10 000 and ClickHouse's 50 000 must be ACCEPTED on
    /// CH and left intact. Before, the CH path would have applied the PG cap.
    #[test]
    fn public_sql_accepts_limit_between_pg_and_ch_caps() {
        let sql = ClickHouseEngine::prepare_public_sql("SELECT num FROM blocks LIMIT 20000", &[], 100)
            .expect("20000 is within the CH cap and must validate");
        assert_eq!(sql, "SELECT num FROM blocks LIMIT 20000");
    }

    /// The generated CH CTE uses unhex/reinterpretAsUInt256/reinterpretAsInt256;
    /// they must be allowlisted or `?signature=` dies on ClickHouse the moment
    /// the validator is applied. This is the test that catches a missing one.
    #[test]
    fn public_sql_accepts_signature_cte_and_caps_outer_query() {
        let sql = ClickHouseEngine::prepare_public_sql("SELECT * FROM Transfer", &[SIG], 100)
            .expect("the CH signature CTE must pass the allowlist validator");
        assert!(sql.starts_with("WITH "), "CTE wrapped; got: {sql}");
        assert!(sql.ends_with("LIMIT 100"), "outer query capped; got: {sql}");
    }

    #[test]
    fn public_sql_signature_with_pushdown_filter_validates() {
        let sql = ClickHouseEngine::prepare_public_sql(
            "SELECT count(*) FROM Transfer WHERE \"to\" = '0xC281cb25715EA8e46c9B916F22aE7c0F55b014d7'",
            &[SIG],
            100,
        )
        .expect("pushdown-rewritten signature query must validate");
        assert!(sql.contains("LIMIT 100"), "got: {sql}");
    }
}
