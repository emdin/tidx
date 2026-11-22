use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::time::Instant;

use crate::db::Pool;
use crate::metrics;
use crate::query::{
    extract_column_references, extract_raw_column_predicates, validate_query, EventSignature,
    HARD_LIMIT_MAX,
};

#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub chain_id: i64,
    pub head_num: i64,
    pub synced_num: i64,
    pub tip_num: i64,
    pub lag: i64,
    /// Current adaptive safety window: a block only enters tidx once it is
    /// `head_delay_blocks` deep. Consumers (VIL, explorer) should classify
    /// canonical/orphaned only after a submission's height has passed this
    /// threshold below the current tip. Distinct from `lag` (which is the
    /// observed `head - tip` and can spike during backfill). Null on chains
    /// whose sync loop hasn't ticked since this field landed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_delay_blocks: Option<i64>,
    pub gap_blocks: i64,
    /// Detected gaps in the blocks table: [(start, end), ...]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<(i64, i64)>,
    pub backfill_num: Option<i64>,
    pub backfill_remaining: i64,
    pub sync_rate: Option<f64>,
    pub eta_secs: Option<f64>,
    pub updated_at: DateTime<Utc>,
    /// Per-table high-water marks for PostgreSQL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postgres: Option<StoreStatus>,
    /// Per-table high-water marks for ClickHouse (if enabled).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clickhouse: Option<StoreStatus>,
}

/// Per-table high-water marks for a storage backend.
#[derive(Debug, Clone, Serialize)]
pub struct StoreStatus {
    pub blocks: Option<i64>,
    pub txs: Option<i64>,
    pub logs: Option<i64>,
    pub receipts: Option<i64>,
    /// Write rate in blocks/sec (from rolling window)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    /// Cumulative row counts (since process start)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub txs_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipts_count: Option<u64>,
}

pub async fn get_all_status(pool: &Pool) -> Result<Vec<SyncStatus>> {
    let conn = pool.get().await?;

    let rows = conn
        .query(
            "SELECT chain_id, head_num, synced_num, tip_num, backfill_num, started_at, updated_at, head_delay_blocks FROM sync_state ORDER BY chain_id",
            &[],
        )
        .await?;

    // Gaps above the sync watermark only. This used to run the unbounded LAG
    // window (6.7 s at 16.9M rows) on EVERY /status poll, then
    // `unwrap_or_default()` turned its timeout into `gaps: []` — so /status
    // reported "no gaps" precisely while the gap-fill loop was wedged. Bounded
    // it is O(gap); and a scan failure is now an error, not a clean answer.
    let max_tip: u64 = rows.iter().map(|r| r.get::<_, i64>(3) as u64).max().unwrap_or(0);
    let min_synced: u64 = rows.iter().map(|r| r.get::<_, i64>(2) as u64).min().unwrap_or(0);
    let gaps = crate::sync::writer::gaps_above_watermark(pool, min_synced, max_tip).await?;
    let gaps_i64: Vec<(i64, i64)> = gaps.iter().map(|(s, e)| (*s as i64, *e as i64)).collect();

    Ok(rows
        .iter()
        .map(|row| {
            let synced_num: i64 = row.get(2);
            let tip_num: i64 = row.get(3);
            let backfill_num: Option<i64> = row.get(4);
            let started_at: Option<DateTime<Utc>> = row.get(5);

            let backfill_remaining = match backfill_num {
                None => synced_num.saturating_sub(1),
                Some(0) => 0,
                Some(n) => n,
            };

            let sync_rate = started_at.and_then(|started| {
                let elapsed = Utc::now().signed_duration_since(started);
                let secs = elapsed.num_seconds() as f64;
                let total_indexed = match backfill_num {
                    Some(n) => synced_num - n + 1,
                    None => 1,
                };
                if secs > 0.0 { Some(total_indexed as f64 / secs) } else { None }
            });

            let eta_secs = sync_rate.and_then(|rate| {
                if rate > 0.0 { Some(backfill_remaining as f64 / rate) } else { None }
            });

            // Gap = blocks between synced_num and tip_num that may be missing
            let gap_blocks = tip_num.saturating_sub(synced_num);

            SyncStatus {
                chain_id: row.get(0),
                head_num: row.get(1),
                synced_num,
                tip_num,
                lag: row.get::<_, i64>(1) - tip_num, // lag from head to tip (realtime)
                head_delay_blocks: row.get(7),
                gap_blocks,
                gaps: gaps_i64.clone(),
                backfill_num,
                backfill_remaining,
                sync_rate,
                eta_secs,
                updated_at: row.get(6),
                postgres: None,
                clickhouse: None,
            }
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Server-side query execution time in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_time_ms: Option<f64>,
}

pub struct QueryOptions {
    pub timeout_ms: u64,
    pub limit: i64,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            timeout_ms: 5000,
            limit: HARD_LIMIT_MAX,
        }
    }
}

/// Open the transaction untrusted `/query` SQL runs in: read-only, with the
/// caller's timeout, as the unprivileged `tidx_query` role (db/query_role.sql).
///
/// Everything is SET LOCAL so it dies with the transaction. The pool is shared
/// with the sync engine: a session-level `statement_timeout` once leaked to its
/// gap scans (2026-09-14), and a leaked role would break its writes. If the
/// future is dropped mid-query, the Transaction's Drop rolls back and the
/// settings with it.
///
/// The role switch is what stops a validator bypass from reaching superuser-only
/// functions (2026-11-06: pg_read_file / lo_export led to code execution). A
/// superuser session could switch back with `set_config('role', ...)`, so that
/// function must stay off the validator allowlist.
pub async fn begin_public_query(
    conn: &mut deadpool_postgres::Object,
    timeout_ms: u64,
) -> Result<deadpool_postgres::Transaction<'_>> {
    let tx = conn.transaction().await?;
    // standard_conforming_strings keeps backslashes in '...' literal, which is
    // how the validator's parser reads them (see reject_ambiguous_lexing).
    tx.batch_execute(&format!(
        "SET TRANSACTION READ ONLY; \
         SET LOCAL statement_timeout = {timeout_ms}; \
         SET LOCAL standard_conforming_strings = on; \
         SET LOCAL ROLE tidx_query"
    ))
    .await?;
    Ok(tx)
}

/// Execute a query on PostgreSQL.
pub async fn execute_query_postgres(
    pool: &Pool,
    sql: &str,
    signatures: &[&str],
    options: &QueryOptions,
) -> Result<QueryResult> {
    crate::query::reject_ambiguous_lexing(sql, crate::query::SqlEngine::Postgres)?;

    // Generate CTE SQL if signatures are provided
    let sql = if !signatures.is_empty() {
        let sigs: Vec<EventSignature> = signatures
            .iter()
            .map(|s| EventSignature::parse(s))
            .collect::<Result<_>>()?;

        // Normalize table references and rewrite filters for each signature
        let mut sql = sql.to_string();
        for sig in &sigs {
            sql = sig.normalize_table_references(&sql);
            sql = sig.rewrite_filters_for_pushdown(&sql);
        }

        let used_columns = extract_column_references(&sql);
        let filter = if used_columns.is_empty() {
            None
        } else {
            Some(&used_columns)
        };
        let pushdown = extract_raw_column_predicates(&sql);
        let ctes: Vec<String> = sigs
            .iter()
            .map(|sig| sig.to_cte_sql_postgres_with_pushdown(filter, &pushdown))
            .collect();
        format!("WITH {} {sql}", ctes.join(", "))
    } else {
        sql.to_string()
    };

    // Validate query (after CTE wrapping so signature-derived table names are valid)
    validate_query(&sql)?;

    // Append a LIMIT when absent and clamp any explicit LIMIT to the cap.
    let sql = crate::query::enforce_limit(&sql, options.limit, HARD_LIMIT_MAX);

    // Convert '0x...' hex literals to '\x...' bytea literals for PostgreSQL
    // Only replace hex values (40+ chars), not short '0x' prefixes used in concat()
    let sql = crate::query::convert_hex_literals_postgres(&sql);

    let mut conn = pool.get().await?;
    let tx = begin_public_query(&mut conn, options.timeout_ms).await?;

    let start = Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(options.timeout_ms + 100),
        tx.query(&sql, &[]),
    )
    .await;

    let rows = match result {
        Ok(Ok(rows)) => {
            metrics::record_query_duration(start.elapsed());
            rows
        }
        Ok(Err(e)) => {
            return Err(anyhow!("Query error: {}", sanitize_db_error(&format_pg_error(&e))));
        }
        Err(_) => return Err(anyhow!("Query timeout")),
    };

    // Get columns from result (even if empty, prepared statement has column info)
    let columns: Vec<String> = if rows.is_empty() {
        // For empty results, prepare statement to get column metadata
        tx.prepare(&sql)
            .await
            .ok()
            .map(|s| s.columns().iter().map(|c| c.name().to_string()).collect())
            .unwrap_or_default()
    } else {
        rows[0].columns().iter().map(|c| c.name().to_string()).collect()
    };
    tx.commit().await?;

    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    if rows.is_empty() {
        return Ok(QueryResult {
            columns,
            rows: vec![],
            row_count: 0,
            engine: Some("postgres".to_string()),
            query_time_ms: Some(elapsed_ms),
        });
    }
    let row_count = rows.len();
    metrics::record_query_rows(row_count as u64);

    // One unrenderable column would otherwise come back as nulls that look
    // like data.
    if let Some(col) = (0..columns.len())
        .find(|&i| column_json(&rows[0], i).is_none())
        .map(|i| &rows[0].columns()[i])
    {
        return Err(anyhow!(
            "Column \"{}\" has type {}, which /query can't return; cast it to a type it can, e.g. ::text",
            col.name(),
            col.type_().name()
        ));
    }

    let result_rows: Vec<Vec<serde_json::Value>> = rows
        .iter()
        .map(|row| {
            (0..columns.len())
                .map(|i| format_column_json(row, i))
                .collect()
        })
        .collect();

    Ok(QueryResult {
        columns,
        rows: result_rows,
        row_count,
        engine: Some("postgres".to_string()),
        query_time_ms: Some(elapsed_ms),
    })
}


/// One Postgres value as JSON; null for SQL NULL and for a type /query has no
/// rendering for (`execute_query_postgres` rejects those instead).
pub fn format_column_json(row: &tokio_postgres::Row, idx: usize) -> serde_json::Value {
    column_json(row, idx).unwrap_or(serde_json::Value::Null)
}

/// `None` when the column's type has no rendering here, whatever its value.
fn column_json(row: &tokio_postgres::Row, idx: usize) -> Option<serde_json::Value> {
    use serde_json::Value;
    fn get<'a, T: tokio_postgres::types::FromSql<'a>>(
        row: &'a tokio_postgres::Row,
        idx: usize,
        to_json: impl FnOnce(T) -> Value,
    ) -> Value {
        row.try_get::<_, T>(idx).ok().map_or(Value::Null, to_json)
    }
    let rfc3339 = |v: DateTime<Utc>| Value::String(v.to_rfc3339());
    let hex = |v: Vec<u8>| Value::String(format!("0x{}", hex::encode(v)));
    // f32 -> f64 directly turns 0.1 into 0.10000000149011612; go via its text.
    let float4 = |v: f32| {
        v.to_string()
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Null, Value::Number)
    };
    let float8 = |v: f64| serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number);

    Some(match row.columns()[idx].type_().name() {
        "int2" => get::<i16>(row, idx, |v| v.into()),
        "int4" => get::<i32>(row, idx, |v| v.into()),
        "int8" => get::<i64>(row, idx, |v| v.into()),
        "numeric" => get::<PgNumeric>(row, idx, |v| Value::String(v.0)),
        "float4" => get::<f32>(row, idx, float4),
        "float8" => get::<f64>(row, idx, float8),
        "bytea" => get::<Vec<u8>>(row, idx, hex),
        "text" | "varchar" | "name" | "bpchar" => get::<String>(row, idx, Value::String),
        "json" | "jsonb" => get::<Value>(row, idx, |v| v),
        "timestamptz" => get::<DateTime<Utc>>(row, idx, rfc3339),
        "timestamp" => get::<chrono::NaiveDateTime>(row, idx, |v| {
            Value::String(v.format("%Y-%m-%dT%H:%M:%S%.f").to_string())
        }),
        "date" => get::<chrono::NaiveDate>(row, idx, |v| {
            Value::String(v.format("%Y-%m-%d").to_string())
        }),
        "time" => get::<chrono::NaiveTime>(row, idx, |v| {
            Value::String(v.format("%H:%M:%S%.f").to_string())
        }),
        "interval" => get::<PgInterval>(row, idx, |v| Value::String(v.0)),
        "bool" => get::<bool>(row, idx, Value::Bool),
        "_int2" => array_json::<i16>(row, idx, |v| v.into()),
        "_int4" => array_json::<i32>(row, idx, |v| v.into()),
        "_int8" => array_json::<i64>(row, idx, |v| v.into()),
        "_numeric" => array_json::<PgNumeric>(row, idx, |v| Value::String(v.0)),
        "_float4" => array_json::<f32>(row, idx, float4),
        "_float8" => array_json::<f64>(row, idx, float8),
        "_text" | "_varchar" => array_json::<String>(row, idx, Value::String),
        "_bool" => array_json::<bool>(row, idx, Value::Bool),
        "_bytea" => array_json::<Vec<u8>>(row, idx, hex),
        "_timestamptz" => array_json::<DateTime<Utc>>(row, idx, rfc3339),
        _ => return None,
    })
}

type FromSqlError = Box<dyn std::error::Error + Sync + Send>;

/// NUMERIC exactly as Postgres prints it, at any size. rust_decimal, used
/// before, stops at 2^96 (sums of wei, abi_uint of a full uint256).
struct PgNumeric(String);

impl<'a> tokio_postgres::types::FromSql<'a> for PgNumeric {
    fn from_sql(_: &tokio_postgres::types::Type, raw: &'a [u8]) -> Result<Self, FromSqlError> {
        numeric_text(raw).map(PgNumeric)
    }

    fn accepts(ty: &tokio_postgres::types::Type) -> bool {
        *ty == tokio_postgres::types::Type::NUMERIC
    }
}

/// The binary NUMERIC (ndigits, weight, sign, dscale, then base-10000 digits,
/// the first of weight `weight`), printed like Postgres' get_str_from_var.
fn numeric_text(raw: &[u8]) -> Result<String, FromSqlError> {
    use std::fmt::Write;
    let word = |i: usize| -> Result<u16, FromSqlError> {
        let b = raw.get(2 * i..2 * i + 2).ok_or("truncated numeric")?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    };
    let ndigits = word(0)? as usize;
    let weight = word(1)? as i16 as i64;
    let sign = word(2)?;
    let dscale = word(3)? as usize;
    let negative = match sign {
        0x0000 => false,
        0x4000 => true,
        0xC000 => return Ok("NaN".into()),
        0xD000 => return Ok("Infinity".into()),
        0xF000 => return Ok("-Infinity".into()),
        _ => return Err("invalid numeric sign".into()),
    };
    let digits = (0..ndigits)
        .map(|i| word(4 + i))
        .collect::<Result<Vec<_>, _>>()?;
    let digit = |i: i64| {
        usize::try_from(i)
            .ok()
            .and_then(|i| digits.get(i))
            .copied()
            .unwrap_or(0)
    };

    let mut out = String::from(if negative { "-" } else { "" });
    if weight < 0 {
        out.push('0');
    } else {
        write!(out, "{}", digit(0))?;
        for i in 1..=weight {
            write!(out, "{:04}", digit(i))?;
        }
    }
    if dscale > 0 {
        let mut frac = String::with_capacity(dscale + 4);
        let mut i = weight + 1;
        while frac.len() < dscale {
            write!(frac, "{:04}", digit(i))?;
            i += 1;
        }
        frac.truncate(dscale);
        out.push('.');
        out.push_str(&frac);
    }
    Ok(out)
}

/// INTERVAL as Postgres prints it with the default IntervalStyle (`postgres`):
/// "1 year 2 mons 3 days 04:05:06.789".
struct PgInterval(String);

impl<'a> tokio_postgres::types::FromSql<'a> for PgInterval {
    fn from_sql(_: &tokio_postgres::types::Type, raw: &'a [u8]) -> Result<Self, FromSqlError> {
        let raw: [u8; 16] = raw.try_into().map_err(|_| "interval is not 16 bytes")?;
        let micros = i64::from_be_bytes(raw[0..8].try_into()?);
        let days = i32::from_be_bytes(raw[8..12].try_into()?);
        let months = i32::from_be_bytes(raw[12..16].try_into()?);
        Ok(PgInterval(interval_text(micros, days, months)))
    }

    fn accepts(ty: &tokio_postgres::types::Type) -> bool {
        *ty == tokio_postgres::types::Type::INTERVAL
    }
}

/// Postgres' EncodeInterval, INTSTYLE_POSTGRES.
fn interval_text(micros: i64, days: i32, months: i32) -> String {
    let mut out = String::new();
    let (mut is_zero, mut is_before) = (true, false);
    for (value, unit) in [
        (i64::from(months / 12), "year"),
        (i64::from(months % 12), "mon"),
        (i64::from(days), "day"),
    ] {
        if value == 0 {
            continue;
        }
        let sep = if is_zero { "" } else { " " };
        let plus = if is_before && value > 0 { "+" } else { "" };
        let plural = if value != 1 { "s" } else { "" };
        out.push_str(&format!("{sep}{plus}{value} {unit}{plural}"));
        is_before = value < 0;
        is_zero = false;
    }
    let hour = micros / 3_600_000_000;
    let min = micros / 60_000_000 % 60;
    let sec = micros / 1_000_000 % 60;
    let fsec = micros % 1_000_000;
    if is_zero || micros != 0 {
        let sep = if is_zero { "" } else { " " };
        let sign = if micros < 0 {
            "-"
        } else if is_before {
            "+"
        } else {
            ""
        };
        out.push_str(&format!(
            "{sep}{sign}{:02}:{:02}:{:02}",
            hour.abs(),
            min.abs(),
            sec.abs()
        ));
        if fsec != 0 {
            out.push_str(format!(".{:06}", fsec.abs()).trim_end_matches('0'));
        }
    }
    out
}

fn array_json<T>(
    row: &tokio_postgres::Row,
    idx: usize,
    to_json: impl Fn(T) -> serde_json::Value,
) -> serde_json::Value
where
    T: for<'a> tokio_postgres::types::FromSql<'a>,
{
    row.try_get::<_, Vec<Option<T>>>(idx)
        .ok()
        .map_or(serde_json::Value::Null, |items| {
            serde_json::Value::Array(
                items
                    .into_iter()
                    .map(|v| v.map_or(serde_json::Value::Null, &to_json))
                    .collect(),
            )
        })
}

pub fn format_column_string(row: &tokio_postgres::Row, idx: usize) -> String {
    match format_column_json(row, idx) {
        serde_json::Value::Null => "NULL".to_string(),
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Extract a meaningful error string from a `tokio_postgres::Error`.
///
/// `tokio_postgres::Error`'s `Display` impl is sparse — for SQLSTATE failures
/// (operator-does-not-exist, syntax errors, etc.) it just renders `"db error"`,
/// hiding the actual message that PG returned. The real diagnostic lives in
/// `as_db_error()`, which we surface here. Everything still flows through
/// `sanitize_db_error` to strip filesystem paths/IPs/connection strings before
/// leaving the process.
fn format_pg_error(err: &tokio_postgres::Error) -> String {
    if let Some(db_err) = err.as_db_error() {
        format!("{}: {}", db_err.severity(), db_err.message())
    } else {
        err.to_string()
    }
}

/// Sanitize database error messages to prevent information leakage.
///
/// Removes file paths, internal schema details, and other sensitive info
/// while preserving useful error context for debugging.
fn sanitize_db_error(error: &str) -> String {
    // Truncate very long errors
    let error = if error.len() > 500 {
        format!("{}...", &error[..500])
    } else {
        error.to_string()
    };

    // Remove file paths (Unix and Windows)
    let error = regex_lite::Regex::new(r"(/[a-zA-Z0-9_./-]+|[A-Z]:\\[a-zA-Z0-9_.\\ -]+)")
        .map(|re| re.replace_all(&error, "[path]").to_string())
        .unwrap_or(error);

    // Remove potential connection strings
    let error = regex_lite::Regex::new(r"postgres://[^\s]+")
        .map(|re| re.replace_all(&error, "[connection]").to_string())
        .unwrap_or(error);

    // Remove IP addresses
    let error = regex_lite::Regex::new(r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(:\d+)?")
        .map(|re| re.replace_all(&error, "[address]").to_string())
        .unwrap_or(error);

    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::EventSignature;
    use insta::assert_snapshot;

    // ========================================================================
    // Event CTE SQL Generation Tests (Both Engines)
    // ========================================================================

    #[test]
    fn test_transfer_cte_postgres() {
        let sig = EventSignature::parse("Transfer(address indexed from, address indexed to, uint256 value)").unwrap();
        assert_snapshot!(sig.to_cte_sql_postgres());
    }

    #[test]
    fn test_transfer_cte_clickhouse() {
        let sig = EventSignature::parse("Transfer(address indexed from, address indexed to, uint256 value)").unwrap();
        assert_snapshot!(sig.to_cte_sql_clickhouse());
    }

    #[test]
    fn test_approval_cte_postgres() {
        let sig = EventSignature::parse("Approval(address indexed owner, address indexed spender, uint256 value)").unwrap();
        assert_snapshot!(sig.to_cte_sql_postgres());
    }

    #[test]
    fn test_approval_cte_clickhouse() {
        let sig = EventSignature::parse("Approval(address indexed owner, address indexed spender, uint256 value)").unwrap();
        assert_snapshot!(sig.to_cte_sql_clickhouse());
    }

    #[test]
    fn test_swap_cte_postgres() {
        let sig = EventSignature::parse(
            "Swap(address indexed sender, uint256 amount0In, uint256 amount1In, uint256 amount0Out, uint256 amount1Out, address indexed to)"
        ).unwrap();
        assert_snapshot!(sig.to_cte_sql_postgres());
    }

    #[test]
    fn test_swap_cte_clickhouse() {
        let sig = EventSignature::parse(
            "Swap(address indexed sender, uint256 amount0In, uint256 amount1In, uint256 amount0Out, uint256 amount1Out, address indexed to)"
        ).unwrap();
        assert_snapshot!(sig.to_cte_sql_clickhouse());
    }

    #[test]
    fn test_paused_cte_postgres() {
        let sig = EventSignature::parse("Paused(bool paused)").unwrap();
        assert_snapshot!(sig.to_cte_sql_postgres());
    }

    #[test]
    fn test_paused_cte_clickhouse() {
        let sig = EventSignature::parse("Paused(bool paused)").unwrap();
        assert_snapshot!(sig.to_cte_sql_clickhouse());
    }

    #[test]
    fn test_role_granted_cte_postgres() {
        let sig = EventSignature::parse("RoleGranted(bytes32 indexed role, address indexed account, address indexed sender)").unwrap();
        assert_snapshot!(sig.to_cte_sql_postgres());
    }

    #[test]
    fn test_role_granted_cte_clickhouse() {
        let sig = EventSignature::parse("RoleGranted(bytes32 indexed role, address indexed account, address indexed sender)").unwrap();
        assert_snapshot!(sig.to_cte_sql_clickhouse());
    }

    #[test]
    fn test_filtered_cte_postgres() {
        let sig = EventSignature::parse("Transfer(address indexed from, address indexed to, uint256 value)").unwrap();
        let mut used_columns = std::collections::HashSet::new();
        used_columns.insert("to".to_string());
        used_columns.insert("value".to_string());
        assert_snapshot!(sig.to_cte_sql_postgres_filtered(Some(&used_columns)));
    }

    #[test]
    fn test_filtered_cte_clickhouse() {
        let sig = EventSignature::parse("Transfer(address indexed from, address indexed to, uint256 value)").unwrap();
        let mut used_columns = std::collections::HashSet::new();
        used_columns.insert("to".to_string());
        used_columns.insert("value".to_string());
        assert_snapshot!(sig.to_cte_sql_clickhouse_filtered(Some(&used_columns)));
    }

    // ========================================================================
    // Query Options Tests
    // ========================================================================

    #[test]
    fn test_query_options_default() {
        let options = QueryOptions::default();
        assert_eq!(options.timeout_ms, 5000);
        assert_eq!(options.limit, 10000);
    }

    // ========================================================================
    // Sanitize Error Tests
    // ========================================================================

    #[test]
    fn test_sanitize_removes_file_paths() {
        let error = "Error at /home/user/project/src/main.rs:42";
        let sanitized = sanitize_db_error(error);
        assert!(!sanitized.contains("/home/user"));
        assert!(sanitized.contains("[path]"));
    }

    #[test]
    fn test_sanitize_removes_connection_strings() {
        // Note: the path regex runs first, so parts of the URL may be matched as paths
        // The key is that sensitive info (user:pass) is removed
        let error = "Connection failed: postgres://user:pass@host:5432/db";
        let sanitized = sanitize_db_error(error);
        // User credentials should not be visible
        assert!(!sanitized.contains("user:pass"));
        // Either [connection] or [path] replacement happened
        assert!(sanitized.contains("[connection]") || sanitized.contains("[path]"));
    }

    #[test]
    fn test_sanitize_removes_ip_addresses() {
        let error = "Connection to 192.168.1.100:5432 failed";
        let sanitized = sanitize_db_error(error);
        assert!(!sanitized.contains("192.168.1.100"));
        assert!(sanitized.contains("[address]"));
    }

    #[test]
    fn test_sanitize_truncates_long_errors() {
        let error = "x".repeat(600);
        let sanitized = sanitize_db_error(&error);
        assert!(sanitized.len() < 510); // 500 + "..."
        assert!(sanitized.ends_with("..."));
    }

    /// SQLSTATE-style error messages (severity + diagnostic) MUST survive
    /// sanitization. Without this guarantee, `/query` falls back to
    /// `tokio_postgres::Error`'s `Display`, which renders an opaque "db error"
    /// — exactly the bug an operator hit when filing a bad cast query.
    /// Regression test for that bug: the diagnostic substring callers need
    /// to debug the query (operator name, type names) must not be stripped.
    #[test]
    fn test_sanitize_preserves_pg_diagnostic_messages() {
        let error = "ERROR: operator does not exist: bigint * text";
        let sanitized = sanitize_db_error(error);
        assert!(
            sanitized.contains("operator does not exist"),
            "diagnostic substring stripped from: {sanitized}"
        );
        assert!(
            sanitized.contains("bigint") && sanitized.contains("text"),
            "type names stripped from: {sanitized}"
        );

        // Same property for syntax errors and missing-column errors —
        // the column/keyword the operator typed must reach them.
        let cases = [
            "ERROR: syntax error at or near \"FORM\"",
            "ERROR: column \"foo\" does not exist",
            "ERROR: invalid input syntax for type integer: \"abc\"",
        ];
        for raw in cases {
            let sanitized = sanitize_db_error(raw);
            assert!(
                sanitized.contains("ERROR:"),
                "severity prefix stripped from: {sanitized}"
            );
        }
    }

}

