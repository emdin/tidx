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
    /// The server's row cap cut the result: more rows matched than returned.
    /// A LIMIT the caller wrote never counts.
    #[serde(default)]
    pub truncated: bool,
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

/// A query stopped by its time limit, on either engine. Typed so callers never
/// have to guess from error text, which can echo the caller's own SQL.
#[derive(Debug, thiserror::Error)]
#[error("query timed out")]
pub struct QueryTimeout;

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

/// Everything the public `/query` path does to untrusted SQL before it reaches
/// Postgres, apart from the row limit: the lexing guard, `?signature=` CTE
/// wrapping, then the allowlist. Mirrors `ClickHouseEngine::prepare_public_sql`.
pub fn prepare_public_sql_postgres(sql: &str, signatures: &[&str]) -> Result<String> {
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
    Ok(sql)
}

/// Execute a query on PostgreSQL.
pub async fn execute_query_postgres(
    pool: &Pool,
    sql: &str,
    signatures: &[&str],
    options: &QueryOptions,
) -> Result<QueryResult> {
    let sql = prepare_public_sql_postgres(sql, signatures)?;

    // Append a LIMIT when absent and clamp any explicit LIMIT to the cap. When
    // the server imposes it, one extra row is fetched to tell a cut result from
    // one that fit.
    let (sql, imposed) = crate::query::enforce_limit_probe(&sql, options.limit, HARD_LIMIT_MAX);

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
        // statement_timeout and lock_timeout cancels, by SQLSTATE: the message
        // text can echo the caller's own SQL.
        Ok(Err(e))
            if matches!(
                e.code(),
                Some(&tokio_postgres::error::SqlState::QUERY_CANCELED)
                    | Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE)
            ) =>
        {
            return Err(QueryTimeout.into());
        }
        Ok(Err(e)) => {
            return Err(anyhow!("Query error: {}", sanitize_db_error(&format_pg_error(&e))));
        }
        Err(_) => return Err(QueryTimeout.into()),
    };

    // Get columns from result (even if empty, prepared statement has column info)
    let columns: Vec<(String, tokio_postgres::types::Type)> = if rows.is_empty() {
        // For empty results, prepare statement to get column metadata
        tx.prepare(&sql)
            .await
            .ok()
            .map(|s| {
                s.columns()
                    .iter()
                    .map(|c| (c.name().to_string(), c.type_().clone()))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        rows[0]
            .columns()
            .iter()
            .map(|c| (c.name().to_string(), c.type_().clone()))
            .collect()
    };
    tx.commit().await?;

    // An unrenderable column or value would otherwise come back as nulls that
    // look like data. Checked by type, so it fails the same with or without rows.
    let renderers = columns
        .iter()
        .map(|(name, ty)| {
            renderer(ty).ok_or_else(|| {
                anyhow!(
                    "Column \"{name}\" has type {}, which /query can't return; cast it to a type it can, e.g. ::text",
                    ty.name()
                )
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let columns: Vec<String> = columns.into_iter().map(|(name, _)| name).collect();

    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    if rows.is_empty() {
        return Ok(QueryResult {
            columns,
            rows: vec![],
            row_count: 0,
            engine: Some("postgres".to_string()),
            query_time_ms: Some(elapsed_ms),
            truncated: false,
        });
    }
    let truncated = imposed.is_some_and(|n| rows.len() as i64 > n);
    let rows = match imposed {
        Some(n) if truncated => &rows[..n as usize],
        _ => &rows[..],
    };
    let row_count = rows.len();
    metrics::record_query_rows(row_count as u64);

    let result_rows = rows
        .iter()
        .map(|row| {
            renderers
                .iter()
                .enumerate()
                .map(|(i, render)| {
                    render(row, i).map_err(|e| {
                        anyhow!(
                            "Column \"{}\" holds a value /query can't return ({e}); cast it, e.g. ::text",
                            columns[i]
                        )
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(QueryResult {
        columns,
        rows: result_rows,
        row_count,
        engine: Some("postgres".to_string()),
        query_time_ms: Some(elapsed_ms),
        truncated,
    })
}

/// One Postgres value as JSON; null for SQL NULL and for anything /query can't
/// render (`execute_query_postgres` rejects those instead).
pub fn format_column_json(row: &tokio_postgres::Row, idx: usize) -> serde_json::Value {
    renderer(row.columns()[idx].type_())
        .and_then(|render| render(row, idx).ok())
        .unwrap_or(serde_json::Value::Null)
}

type Render = fn(&tokio_postgres::Row, usize) -> Result<serde_json::Value, FromSqlError>;

/// How /query renders a value of `ty`, or `None` for a type it can't. A value
/// its decoder rejects is an error, never a null.
fn renderer(ty: &tokio_postgres::types::Type) -> Option<Render> {
    use serde_json::Value;
    let render: Render = match ty.name() {
        "int2" => |r, i| one::<i16>(r, i, Value::from),
        "int4" => |r, i| one::<i32>(r, i, Value::from),
        "int8" => |r, i| one::<i64>(r, i, Value::from),
        "numeric" => |r, i| one::<PgNumeric>(r, i, |v| Value::String(v.0)),
        "float4" => |r, i| one::<f32>(r, i, float4_json),
        "float8" => |r, i| one::<f64>(r, i, float_json),
        "bytea" => |r, i| one::<Vec<u8>>(r, i, hex_json),
        "text" | "varchar" | "name" | "bpchar" => |r, i| one::<String>(r, i, Value::String),
        // The JSON text, as ClickHouse returns its String `calls`; parsing it
        // would round big numbers to f64 and turn 1e400 into null.
        "json" | "jsonb" => |r, i| one::<PgJson>(r, i, |v| Value::String(v.0)),
        "timestamptz" | "timestamp" => |r, i| one::<PgTimestamp>(r, i, |v| Value::String(v.0)),
        "date" => |r, i| one::<PgDate>(r, i, |v| Value::String(v.0)),
        "time" => |r, i| {
            one::<chrono::NaiveTime>(r, i, |v| Value::String(v.format("%H:%M:%S%.f").to_string()))
        },
        "interval" => |r, i| one::<PgInterval>(r, i, |v| Value::String(v.0)),
        "bool" => |r, i| one::<bool>(r, i, Value::Bool),
        "_int2" => |r, i| many::<i16>(r, i, Value::from),
        "_int4" => |r, i| many::<i32>(r, i, Value::from),
        "_int8" => |r, i| many::<i64>(r, i, Value::from),
        "_numeric" => |r, i| many::<PgNumeric>(r, i, |v| Value::String(v.0)),
        "_float4" => |r, i| many::<f32>(r, i, float4_json),
        "_float8" => |r, i| many::<f64>(r, i, float_json),
        "_text" | "_varchar" => |r, i| many::<String>(r, i, Value::String),
        "_bool" => |r, i| many::<bool>(r, i, Value::Bool),
        "_bytea" => |r, i| many::<Vec<u8>>(r, i, hex_json),
        "_timestamptz" => |r, i| many::<PgTimestamp>(r, i, |v| Value::String(v.0)),
        _ => return None,
    };
    Some(render)
}

fn one<'a, T: tokio_postgres::types::FromSql<'a>>(
    row: &'a tokio_postgres::Row,
    idx: usize,
    to_json: impl FnOnce(T) -> serde_json::Value,
) -> Result<serde_json::Value, FromSqlError> {
    Ok(row
        .try_get::<_, Option<T>>(idx)?
        .map_or(serde_json::Value::Null, to_json))
}

fn many<'a, T: tokio_postgres::types::FromSql<'a>>(
    row: &'a tokio_postgres::Row,
    idx: usize,
    to_json: impl Fn(T) -> serde_json::Value,
) -> Result<serde_json::Value, FromSqlError> {
    Ok(row
        .try_get::<_, Option<Vec<Option<T>>>>(idx)?
        .map_or(serde_json::Value::Null, |items| {
            serde_json::Value::Array(
                items
                    .into_iter()
                    .map(|v| v.map_or(serde_json::Value::Null, &to_json))
                    .collect(),
            )
        }))
}

fn hex_json(v: Vec<u8>) -> serde_json::Value {
    serde_json::Value::String(format!("0x{}", hex::encode(v)))
}

/// Non-finite floats as Postgres spells them, like numeric's.
fn float_json(v: f64) -> serde_json::Value {
    match serde_json::Number::from_f64(v) {
        Some(n) => serde_json::Value::Number(n),
        None if v.is_nan() => serde_json::Value::String("NaN".into()),
        None if v > 0.0 => serde_json::Value::String("Infinity".into()),
        None => serde_json::Value::String("-Infinity".into()),
    }
}

/// `v as f64` would turn 0.1 into 0.10000000149011612; go via its text.
fn float4_json(v: f32) -> serde_json::Value {
    float_json(v.to_string().parse().unwrap_or(f64::from(v)))
}

/// Microseconds (timestamps) and days (dates) count from 2000-01-01.
const PG_EPOCH_UNIX_MICROS: i64 = 946_684_800_000_000;

/// timestamptz as RFC 3339 (the format /query has always used), timestamp
/// the same without the offset, and Postgres' `infinity` / `-infinity`.
struct PgTimestamp(String);

impl<'a> tokio_postgres::types::FromSql<'a> for PgTimestamp {
    fn from_sql(ty: &tokio_postgres::types::Type, raw: &'a [u8]) -> Result<Self, FromSqlError> {
        let micros = i64::from_be_bytes(raw.try_into().map_err(|_| "timestamp is not 8 bytes")?);
        let text = match micros {
            i64::MAX => "infinity".to_string(),
            i64::MIN => "-infinity".to_string(),
            _ => {
                let t = micros
                    .checked_add(PG_EPOCH_UNIX_MICROS)
                    .and_then(DateTime::<Utc>::from_timestamp_micros)
                    .ok_or("timestamp out of range")?;
                if *ty == tokio_postgres::types::Type::TIMESTAMPTZ {
                    t.to_rfc3339()
                } else {
                    t.naive_utc().format("%Y-%m-%dT%H:%M:%S%.f").to_string()
                }
            }
        };
        Ok(PgTimestamp(text))
    }

    fn accepts(ty: &tokio_postgres::types::Type) -> bool {
        matches!(
            *ty,
            tokio_postgres::types::Type::TIMESTAMPTZ | tokio_postgres::types::Type::TIMESTAMP
        )
    }
}

/// date as YYYY-MM-DD, and Postgres' `infinity` / `-infinity`.
struct PgDate(String);

impl<'a> tokio_postgres::types::FromSql<'a> for PgDate {
    fn from_sql(_: &tokio_postgres::types::Type, raw: &'a [u8]) -> Result<Self, FromSqlError> {
        let days = i32::from_be_bytes(raw.try_into().map_err(|_| "date is not 4 bytes")?);
        let text = match days {
            i32::MAX => "infinity".to_string(),
            i32::MIN => "-infinity".to_string(),
            _ => chrono::NaiveDate::from_ymd_opt(2000, 1, 1)
                .and_then(|epoch| epoch.checked_add_signed(chrono::Duration::days(days.into())))
                .ok_or("date out of range")?
                .format("%Y-%m-%d")
                .to_string(),
        };
        Ok(PgDate(text))
    }

    fn accepts(ty: &tokio_postgres::types::Type) -> bool {
        *ty == tokio_postgres::types::Type::DATE
    }
}

/// json / jsonb as the JSON text Postgres holds (jsonb's wire form is a
/// version byte, 1, then that text).
struct PgJson(String);

impl<'a> tokio_postgres::types::FromSql<'a> for PgJson {
    fn from_sql(ty: &tokio_postgres::types::Type, raw: &'a [u8]) -> Result<Self, FromSqlError> {
        let raw = if *ty == tokio_postgres::types::Type::JSONB {
            match raw.split_first() {
                Some((1, text)) => text,
                _ => return Err("unsupported jsonb version".into()),
            }
        } else {
            raw
        };
        Ok(PgJson(std::str::from_utf8(raw)?.to_string()))
    }

    fn accepts(ty: &tokio_postgres::types::Type) -> bool {
        matches!(
            *ty,
            tokio_postgres::types::Type::JSON | tokio_postgres::types::Type::JSONB
        )
    }
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

