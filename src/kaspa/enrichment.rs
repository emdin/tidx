//! Pure logic for `tidx enrich-l1-senders`. Lives in the library so
//! integration tests in `tests/` can exercise the full flow against an
//! ephemeral postgres + a local fake HTTP server.
//!
//! The CLI in `src/cli/enrich_l1_senders.rs` is a thin shim around the
//! `enrich_table` function here.

use anyhow::{Context, Result};
use reqwest::Client;
use reqwest::StatusCode;
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::db;

/// HTTP retry policy for `fetch_senders`. Tuned for api.kaspa.org:
/// transient failures (5xx, 429, timeouts) get exponential backoff up to
/// `max_attempts`; permanent failures (4xx other than 408/429) abort
/// immediately. Defaults are conservative — adjust if running at scale.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl RetryPolicy {
    pub const fn default_polite() -> Self {
        Self {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
        }
    }

    /// Compute the delay to wait before retry attempt `n` (0-indexed).
    /// Pure function — testable, deterministic. No jitter; tests can pin
    /// exact values without flakes.
    pub fn delay_for_attempt(&self, n: u32) -> Duration {
        let factor = 2u64.saturating_pow(n);
        let scaled = self
            .initial_backoff
            .saturating_mul(factor.min(u32::MAX as u64) as u32);
        scaled.min(self.max_backoff)
    }
}

/// Decide whether an HTTP status warrants a retry. The exhaustive list:
///   - 408 Request Timeout (transient, server side)
///   - 429 Too Many Requests (rate limit; backoff is the right move)
///   - 500-599 server errors (transient by convention)
/// Everything else is "permanent" — retrying won't change the outcome.
pub fn is_retriable_status(s: StatusCode) -> bool {
    s == StatusCode::REQUEST_TIMEOUT
        || s == StatusCode::TOO_MANY_REQUESTS
        || s.is_server_error()
}

/// Subset of api.kaspa.org's `/transactions/{id}` (and the batch
/// `/transactions/search`) response that we care about. Other fields
/// (verbose data, signature scripts, mass, etc.) are intentionally not
/// bound — serde will ignore them.
#[derive(Deserialize, Debug, PartialEq, Eq)]
pub struct ApiTransaction {
    /// Present in batch responses — used to key results back to the
    /// requested txids (response order is NOT guaranteed).
    #[serde(default)]
    pub transaction_id: Option<String>,
    #[serde(default)]
    pub inputs: Vec<ApiInput>,
    #[serde(default)]
    pub outputs: Vec<ApiOutput>,
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
pub struct ApiInput {
    #[serde(default)]
    pub previous_outpoint_address: Option<String>,
    #[serde(default)]
    pub previous_outpoint_amount: Option<i64>,
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
pub struct ApiOutput {
    #[serde(default)]
    pub amount: Option<i64>,
}

/// Build the api.kaspa.org URL for resolving one tx's inputs (with
/// previous-outpoint address + amount) AND its outputs (with amounts).
/// One request populates both senders (`l1_senders`) and fee
/// (`l1_fee_sompi = sum(inputs) - sum(outputs)`).
pub fn build_tx_url(rest_base: &str, txid: &[u8]) -> String {
    format!(
        "{}/transactions/{}?inputs=true&outputs=true&resolve_previous_outpoints=light",
        rest_base.trim_end_matches('/'),
        hex::encode(txid),
    )
}

/// Batch endpoint: one POST resolves up to [`MAX_SEARCH_BATCH`] txs with
/// previous outpoints, ~1000× cheaper per row than the per-tx GET (probed
/// 2026-08-08: 500 cold-storage txs in 0.75s vs ~1s per tx on the GET
/// path — the GET cost is per-request overhead, not data temperature).
pub fn build_search_url(rest_base: &str) -> String {
    format!(
        "{}/transactions/search?resolve_previous_outpoints=light",
        rest_base.trim_end_matches('/'),
    )
}

/// Server-side cap on /transactions/search batch size (kaspa-rest-server
/// accepts up to 1000; we stay at 500 — verified live, comfortable margin).
pub const MAX_SEARCH_BATCH: usize = 500;

/// JSON body for the batch search request. Pure for testability.
pub fn build_search_body(txids: &[Vec<u8>]) -> String {
    let ids: Vec<String> = txids.iter().map(hex::encode).collect();
    serde_json::json!({ "transactionIds": ids }).to_string()
}

/// Build the per-row UPDATE SQL for a given table. The table name is
/// interpolated (allowed because callers pass a known table name from a
/// CLI enum, not user-supplied free text), so this isn't a SQL-injection
/// vector. Returned as a `String` for testability.
///
/// COALESCE lets each field be set only if it's currently NULL — so a row
/// that already has senders but no fee gets its fee filled without
/// re-touching the sender data (and vice versa). Matching WHERE clause
/// picks up rows missing EITHER field.
pub fn build_update_sql(table: &str) -> String {
    format!(
        "UPDATE {table}
         SET l1_senders             = COALESCE(l1_senders, $2),
             l1_sender_amounts_sompi = COALESCE(l1_sender_amounts_sompi, $3),
             l1_enriched_at          = COALESCE(l1_enriched_at, now()),
             l1_fee_sompi            = COALESCE(l1_fee_sompi, $4),
             l1_fee_enriched_at      = COALESCE(l1_fee_enriched_at, now())
         WHERE kaspa_txid = $1
           AND (l1_senders IS NULL OR l1_fee_sompi IS NULL)"
    )
}

/// Compute the miner fee from a fetched tx: sum(inputs.previous_outpoint_amount)
/// - sum(outputs.amount). Returns `None` if the arithmetic underflows (coinbase
/// tx, where sum(inputs) = 0 < sum(outputs)) or if the tx has no inputs at all
/// (also coinbase-shaped). Missing per-field amounts are treated as 0, which is
/// the safe convention for the light-resolution response.
pub fn compute_fee_from_api_tx(tx: &ApiTransaction) -> Option<i64> {
    if tx.inputs.is_empty() {
        return None;
    }
    let sum_in: i64 = tx.inputs.iter().map(|i| i.previous_outpoint_amount.unwrap_or(0)).sum();
    let sum_out: i64 = tx.outputs.iter().map(|o| o.amount.unwrap_or(0)).sum();
    if sum_out > sum_in {
        return None;
    }
    Some(sum_in - sum_out)
}

/// Data extracted from one tx's api.kaspa.org response, ready to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichedTx {
    pub senders: Vec<String>,
    pub sender_amounts: Vec<i64>,
    /// `None` for coinbase (no inputs) — the caller skips writing the fee
    /// column in that case.
    pub fee_sompi: Option<i64>,
}

/// Fetch one tx's resolved sender + output data from api.kaspa.org and
/// derive `l1_senders` + `l1_fee_sompi`. On non-2xx HTTP, returns `Err`
/// so the caller can soft-skip and retry on next pass.
///
/// Convenience wrapper using the default retry policy.
pub async fn fetch_enrichment(
    client: &Client,
    url: &str,
    txid: &[u8],
) -> Result<EnrichedTx> {
    fetch_enrichment_with_retry(client, url, txid, RetryPolicy::default_polite()).await
}

/// Same as `fetch_enrichment` but with an explicit retry policy. Transient
/// errors (5xx, 408, 429, network errors) are retried with exponential
/// backoff up to `policy.max_attempts`; permanent errors (other 4xx)
/// abort immediately.
pub async fn fetch_enrichment_with_retry(
    client: &Client,
    url: &str,
    txid: &[u8],
    policy: RetryPolicy,
) -> Result<EnrichedTx> {
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..policy.max_attempts {
        match try_fetch_enrichment_once(client, url, txid).await {
            Ok(parsed) => return Ok(parsed),
            Err(FetchError::Permanent(e)) => return Err(e),
            Err(FetchError::Transient(e)) => {
                last_err = Some(e);
                if attempt + 1 < policy.max_attempts {
                    let delay = policy.delay_for_attempt(attempt);
                    debug!(
                        attempt = attempt + 1,
                        max_attempts = policy.max_attempts,
                        sleep_ms = delay.as_millis() as u64,
                        url = %url,
                        "transient fetch error; backing off"
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("fetch_enrichment: out of retries with no error")))
}

/// Internal classification of fetch errors so the retry loop knows
/// which to keep trying and which to bail on immediately.
enum FetchError {
    /// 4xx (other than 408/429), JSON parse failure — won't be different next time.
    Permanent(anyhow::Error),
    /// 5xx, 408, 429, network/timeout — worth retrying.
    Transient(anyhow::Error),
}

async fn try_fetch_enrichment_once(
    client: &Client,
    url: &str,
    txid: &[u8],
) -> std::result::Result<EnrichedTx, FetchError> {
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        // Network-level errors (DNS, connect, etc) — transient
        Err(e) => return Err(FetchError::Transient(anyhow::anyhow!(e))),
    };
    let status = resp.status();
    if !status.is_success() {
        let msg = format!("HTTP {} for tx {}", status, hex::encode(txid));
        return if is_retriable_status(status) {
            Err(FetchError::Transient(anyhow::anyhow!(msg)))
        } else {
            Err(FetchError::Permanent(anyhow::anyhow!(msg)))
        };
    }
    let parsed: ApiTransaction = match resp.json().await.context("parse api.kaspa.org json") {
        Ok(p) => p,
        // JSON parse error on a 200 — server returned garbage; retry might
        // help if it's a transient flakiness. Treat as transient.
        Err(e) => return Err(FetchError::Transient(e)),
    };
    Ok(extract_enriched(&parsed))
}

/// Project an `ApiTransaction` into the writable `EnrichedTx`. Shared by
/// the single-tx and batch fetch paths.
fn extract_enriched(tx: &ApiTransaction) -> EnrichedTx {
    let mut senders = Vec::with_capacity(tx.inputs.len());
    let mut sender_amounts = Vec::with_capacity(tx.inputs.len());
    for input in &tx.inputs {
        senders.push(input.previous_outpoint_address.clone().unwrap_or_default());
        sender_amounts.push(input.previous_outpoint_amount.unwrap_or(0));
    }
    let fee_sompi = compute_fee_from_api_tx(tx);
    EnrichedTx { senders, sender_amounts, fee_sompi }
}

/// Fetch a whole batch of txs via `POST /transactions/search`. Returns a
/// map keyed by the raw txid bytes — the response order is not guaranteed
/// and txs unknown to the API are simply absent from the response, so the
/// caller diffs the map against its request list to find misses.
///
/// Rate-limit safety: this is ONE request regardless of batch size; the
/// caller runs batches strictly sequentially with an inter-batch delay.
/// Retries use the same policy/classification as the single-tx path —
/// 429/5xx back off exponentially, other 4xx are permanent.
pub async fn fetch_enrichment_batch(
    client: &Client,
    search_url: &str,
    txids: &[Vec<u8>],
    policy: RetryPolicy,
) -> Result<std::collections::HashMap<Vec<u8>, EnrichedTx>> {
    let body = build_search_body(txids);
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..policy.max_attempts {
        match try_fetch_batch_once(client, search_url, &body).await {
            Ok(list) => {
                let mut out = std::collections::HashMap::with_capacity(list.len());
                for tx in &list {
                    let Some(id_hex) = &tx.transaction_id else {
                        continue;
                    };
                    let Ok(id) = hex::decode(id_hex) else {
                        continue;
                    };
                    out.insert(id, extract_enriched(tx));
                }
                return Ok(out);
            }
            Err(FetchError::Permanent(e)) => return Err(e),
            Err(FetchError::Transient(e)) => {
                last_err = Some(e);
                if attempt + 1 < policy.max_attempts {
                    let delay = policy.delay_for_attempt(attempt);
                    debug!(
                        attempt = attempt + 1,
                        max_attempts = policy.max_attempts,
                        sleep_ms = delay.as_millis() as u64,
                        batch = txids.len(),
                        "transient batch fetch error; backing off"
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_err
        .unwrap_or_else(|| anyhow::anyhow!("fetch_enrichment_batch: out of retries with no error")))
}

async fn try_fetch_batch_once(
    client: &Client,
    url: &str,
    body: &str,
) -> std::result::Result<Vec<ApiTransaction>, FetchError> {
    let resp = match client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Err(FetchError::Transient(anyhow::anyhow!(e))),
    };
    let status = resp.status();
    if !status.is_success() {
        let msg = format!("HTTP {status} for batch search");
        return if is_retriable_status(status) {
            Err(FetchError::Transient(anyhow::anyhow!(msg)))
        } else {
            Err(FetchError::Permanent(anyhow::anyhow!(msg)))
        };
    }
    match resp.json().await.context("parse api.kaspa.org batch json") {
        Ok(list) => Ok(list),
        Err(e) => Err(FetchError::Transient(e)),
    }
}

/// Outcome of a full enrichment run on one table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnrichmentStats {
    pub enriched: usize,
    pub failed: usize,
    pub no_inputs: usize,
}

/// Walk all rows in `table` where `l1_senders IS NULL`, fetch each tx's
/// resolved sender data from `rest_base`, and UPDATE the row. Idempotent
/// — re-runs after partial failures or new rows pick up only the still-NULL
/// rows.
/// Delay between successful batch requests. One batch = one HTTP request
/// for up to [`MAX_SEARCH_BATCH`] rows; at 500ms spacing that's ~2 req/s —
/// the same request rate the old per-tx path ran at concurrency=2, but
/// moving 500× the rows per request. Politeness first: we could go
/// faster (probed 10 back-to-back batches with zero 429s), but there's no
/// need — this still finishes 4.5M rows in ~2-3 hours.
const INTER_BATCH_DELAY: Duration = Duration::from_millis(500);

/// Cool-down after a batch fails all its retries (e.g. sustained 429 or
/// 5xx storm). Long pause instead of hammering the next batch into the
/// same wall.
const BATCH_FAILURE_COOLDOWN: Duration = Duration::from_secs(60);

/// Walk all rows in `table` missing senders or fee, resolve them via the
/// batch search endpoint, and UPDATE each row. Idempotent — re-runs pick
/// up only still-NULL rows.
///
/// Keyset pagination on `(created_at, kaspa_txid)` descending — NOT a bare
/// `WHERE ... IS NULL LIMIT n` re-poll. Rows the API doesn't return (or
/// that legitimately have no computable fee) stay NULL, and a re-poll
/// design would re-select them forever and never advance past the first
/// stuck batch. The cursor guarantees forward progress; stragglers get
/// re-attempted on the NEXT full run, not in this one.
pub async fn enrich_table(
    pool: &db::Pool,
    client: &Client,
    rest_base: &str,
    table: &str,
    _concurrency: usize,
    batch_size: usize,
    max_rows: Option<usize>,
) -> Result<EnrichmentStats> {
    info!(table = %table, "starting enrichment scan (batch mode)");
    let mut stats = EnrichmentStats::default();
    let search_url = build_search_url(rest_base);
    let req_batch = batch_size.clamp(1, MAX_SEARCH_BATCH);
    // (created_at, kaspa_txid) keyset cursor; None = start from newest.
    let mut cursor: Option<(chrono::DateTime<chrono::Utc>, Vec<u8>)> = None;

    loop {
        let conn = pool.get().await?;
        let rows = match &cursor {
            None => {
                conn.query(
                    &format!(
                        "SELECT kaspa_txid, created_at FROM {table}
                         WHERE l1_senders IS NULL OR l1_fee_sompi IS NULL
                         ORDER BY created_at DESC, kaspa_txid DESC
                         LIMIT $1"
                    ),
                    &[&(req_batch as i64)],
                )
                .await?
            }
            Some((ts, id)) => {
                conn.query(
                    &format!(
                        "SELECT kaspa_txid, created_at FROM {table}
                         WHERE (l1_senders IS NULL OR l1_fee_sompi IS NULL)
                           AND (created_at, kaspa_txid) < ($2, $3)
                         ORDER BY created_at DESC, kaspa_txid DESC
                         LIMIT $1"
                    ),
                    &[&(req_batch as i64), ts, &id.as_slice()],
                )
                .await?
            }
        };
        drop(conn);

        if rows.is_empty() {
            info!(table = %table, "no more rows to enrich");
            break;
        }
        let txids: Vec<Vec<u8>> = rows.iter().map(|r| r.get::<_, Vec<u8>>(0)).collect();
        let last = rows.last().unwrap();
        cursor = Some((last.get::<_, chrono::DateTime<chrono::Utc>>(1), last.get::<_, Vec<u8>>(0)));

        // One POST for the whole batch, strictly sequential.
        match fetch_enrichment_batch(client, &search_url, &txids, RetryPolicy::default_polite())
            .await
        {
            Ok(resolved) => {
                let misses = txids.len() - resolved.len();
                stats.failed += misses;
                let mut updates: Vec<(&Vec<u8>, &EnrichedTx)> = Vec::with_capacity(resolved.len());
                for txid in &txids {
                    if let Some(e) = resolved.get(txid) {
                        if e.senders.is_empty() {
                            stats.no_inputs += 1;
                        }
                        updates.push((txid, e));
                    }
                }

                if !updates.is_empty() {
                    let mut conn = pool.get().await?;
                    let tx = conn.transaction().await?;
                    let sql = build_update_sql(table);
                    for (txid, e) in &updates {
                        tx.execute(
                            &sql,
                            &[&txid.as_slice(), &e.senders, &e.sender_amounts, &e.fee_sompi],
                        )
                        .await?;
                    }
                    tx.commit().await?;
                    stats.enriched += updates.len();
                }
                tokio::time::sleep(INTER_BATCH_DELAY).await;
            }
            Err(e) => {
                // Whole batch failed after retries — likely rate limiting or
                // an API outage. Cool down hard before the next batch; the
                // cursor has already advanced so we don't re-hit the same
                // rows this run.
                warn!(
                    err = %e,
                    batch = txids.len(),
                    cooldown_secs = BATCH_FAILURE_COOLDOWN.as_secs(),
                    "batch fetch failed after retries; cooling down"
                );
                stats.failed += txids.len();
                tokio::time::sleep(BATCH_FAILURE_COOLDOWN).await;
            }
        }

        info!(
            table = %table,
            batch_size = txids.len(),
            enriched = stats.enriched,
            failed = stats.failed,
            no_inputs = stats.no_inputs,
            "batch done"
        );

        if let Some(cap) = max_rows {
            if stats.enriched >= cap {
                info!(table = %table, cap, "max_rows reached; stopping");
                break;
            }
        }
    }

    info!(
        table = %table,
        ?stats,
        "enrichment finished"
    );
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    // ---------- serde shape ----------

    #[test]
    fn parses_api_response_shape() {
        let json = r#"{
            "inputs": [
                {
                    "transaction_id": "97b167d4...",
                    "index": 0,
                    "previous_outpoint_hash": "97b1dd88...",
                    "previous_outpoint_index": "0",
                    "previous_outpoint_address": "kaspa:qq5xkhfdmm4zzwc25udlmkcg24vefhc54snklphd3slrvcrexspcg40fvxxh4",
                    "previous_outpoint_amount": 10985439355
                }
            ]
        }"#;
        let parsed: ApiTransaction = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.inputs.len(), 1);
        assert_eq!(
            parsed.inputs[0].previous_outpoint_address.as_deref(),
            Some("kaspa:qq5xkhfdmm4zzwc25udlmkcg24vefhc54snklphd3slrvcrexspcg40fvxxh4")
        );
        assert_eq!(parsed.inputs[0].previous_outpoint_amount, Some(10985439355));
    }

    #[test]
    fn parses_api_response_with_missing_address_fields() {
        let json = r#"{ "inputs": [ { "transaction_id": "abc", "index": 0 } ] }"#;
        let parsed: ApiTransaction = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.inputs.len(), 1);
        assert_eq!(parsed.inputs[0].previous_outpoint_address, None);
        assert_eq!(parsed.inputs[0].previous_outpoint_amount, None);
    }

    #[test]
    fn parses_api_response_zero_inputs() {
        let json = r#"{ "inputs": [] }"#;
        let parsed: ApiTransaction = serde_json::from_str(json).unwrap();
        assert!(parsed.inputs.is_empty());
    }

    // ---------- URL construction ----------

    #[test]
    fn builds_canonical_tx_url_without_trailing_slash() {
        let txid =
            hex::decode("97b167d4318621a9abb91003b2d5bd1a6f20aa638124644b356faecbb13c4f5e")
                .unwrap();
        let url = build_tx_url("https://api.kaspa.org", &txid);
        assert_eq!(
            url,
            "https://api.kaspa.org/transactions/97b167d4318621a9abb91003b2d5bd1a6f20aa638124644b356faecbb13c4f5e?inputs=true&outputs=true&resolve_previous_outpoints=light"
        );
    }

    #[test]
    fn builds_tx_url_strips_trailing_slash_on_base() {
        let txid = hex::decode("aa").unwrap();
        let url = build_tx_url("https://api.kaspa.org/", &txid);
        assert!(!url.contains("//transactions"), "double-slash: {url}");
        assert_eq!(
            url,
            "https://api.kaspa.org/transactions/aa?inputs=true&outputs=true&resolve_previous_outpoints=light"
        );
    }

    // ---------- UPDATE SQL ----------

    #[test]
    fn build_update_sql_targets_the_named_table() {
        let sql = build_update_sql("kaspa_l2_submissions");
        assert!(sql.contains("UPDATE kaspa_l2_submissions"), "{sql}");
        assert!(sql.contains("l1_senders"), "{sql}");
        assert!(sql.contains("l1_sender_amounts_sompi"), "{sql}");
        assert!(sql.contains("l1_enriched_at"), "{sql}");
        assert!(sql.contains("l1_fee_sompi"), "{sql}");
        assert!(sql.contains("l1_fee_enriched_at"), "{sql}");
        assert!(sql.contains("WHERE kaspa_txid = $1"), "{sql}");
        // Idempotency: only touch rows still missing at least one column;
        // COALESCE preserves already-populated fields.
        assert!(sql.contains("l1_senders IS NULL"), "{sql}");
        assert!(sql.contains("l1_fee_sompi IS NULL"), "{sql}");
        assert!(sql.contains("COALESCE(l1_senders"), "{sql}");
        assert!(sql.contains("COALESCE(l1_fee_sompi"), "{sql}");
    }

    #[test]
    fn build_update_sql_works_for_kaspa_entries_too() {
        let sql = build_update_sql("kaspa_entries");
        assert!(sql.contains("UPDATE kaspa_entries"), "{sql}");
        assert!(sql.contains("l1_senders IS NULL"), "{sql}");
        assert!(sql.contains("l1_fee_sompi IS NULL"), "{sql}");
    }

    // ---------- fetch_senders end-to-end (local TCP) ----------

    /// Spawn a tiny one-shot HTTP/1.1 server that returns one canned
    /// response and closes. Avoids pulling a mock-server crate as a dep.
    pub(crate) async fn spawn_canned_http_server(
        body: &'static [u8],
        status: &'static str,
    ) -> std::net::SocketAddr {
        use tokio::io::AsyncReadExt as _;
        use tokio::io::AsyncWriteExt as _;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let mut resp = Vec::new();
            write!(
                resp,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            resp.extend_from_slice(body);
            let _ = sock.write_all(&resp).await;
            let _ = sock.flush().await;
        });
        addr
    }

    #[tokio::test]
    async fn fetch_enrichment_round_trips_real_api_shape() {
        // Real tx: 1 input at 10985439355 sompi, 1 output at 10985437381,
        // fee = 1974 sompi.
        let body = br#"{
            "inputs": [
                {
                    "transaction_id": "97b167d4...",
                    "previous_outpoint_address": "kaspa:qq5xkhfdmm4zzwc25udlmkcg24vefhc54snklphd3slrvcrexspcg40fvxxh4",
                    "previous_outpoint_amount": 10985439355
                }
            ],
            "outputs": [
                { "amount": 10985437381 }
            ]
        }"#;
        let addr = spawn_canned_http_server(body, "200 OK").await;
        let url = format!("http://{addr}/transactions/foo");
        let client = Client::builder().build().unwrap();
        let txid = vec![0x97, 0xb1];
        let e = fetch_enrichment(&client, &url, &txid).await.unwrap();
        assert_eq!(e.senders.len(), 1);
        assert_eq!(e.sender_amounts, vec![10985439355i64]);
        assert_eq!(
            e.senders[0],
            "kaspa:qq5xkhfdmm4zzwc25udlmkcg24vefhc54snklphd3slrvcrexspcg40fvxxh4"
        );
        assert_eq!(e.fee_sompi, Some(1974));
    }

    #[tokio::test]
    async fn fetch_enrichment_handles_404_as_error() {
        let addr = spawn_canned_http_server(b"not found", "404 Not Found").await;
        let url = format!("http://{addr}/transactions/foo");
        let client = Client::builder().build().unwrap();
        let txid = vec![0x97, 0xb1];
        let result = fetch_enrichment(&client, &url, &txid).await;
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("HTTP 404"), "expected HTTP 404 in: {msg}");
    }

    #[tokio::test]
    async fn fetch_enrichment_handles_coinbase_shape() {
        // Zero inputs + non-zero outputs = coinbase. Fee undefined.
        let body = br#"{ "inputs": [], "outputs": [ {"amount": 50000000000} ] }"#;
        let addr = spawn_canned_http_server(body, "200 OK").await;
        let url = format!("http://{addr}/transactions/foo");
        let client = Client::builder().build().unwrap();
        let txid = vec![0x97, 0xb1];
        let e = fetch_enrichment(&client, &url, &txid).await.unwrap();
        assert!(e.senders.is_empty());
        assert!(e.sender_amounts.is_empty());
        assert_eq!(e.fee_sompi, None);
    }

    #[tokio::test]
    async fn fetch_enrichment_substitutes_defaults_for_missing_optional_fields() {
        let body = br#"{
            "inputs":  [ {}, {"previous_outpoint_amount": 42} ],
            "outputs": [ {}, {"amount": 5} ]
        }"#;
        let addr = spawn_canned_http_server(body, "200 OK").await;
        let url = format!("http://{addr}/transactions/foo");
        let client = Client::builder().build().unwrap();
        let txid = vec![0x97, 0xb1];
        let e = fetch_enrichment(&client, &url, &txid).await.unwrap();
        assert_eq!(e.senders, vec!["".to_string(), "".to_string()]);
        assert_eq!(e.sender_amounts, vec![0i64, 42i64]);
        assert_eq!(e.fee_sompi, Some(37)); // sum(0+42) - sum(0+5) = 37
    }

    // ---------- compute_fee_from_api_tx (pure) ----------

    #[test]
    fn compute_fee_normal_positive() {
        let tx = ApiTransaction { transaction_id: None,
            inputs: vec![ApiInput {
                previous_outpoint_address: Some("k".into()),
                previous_outpoint_amount: Some(1000),
            }],
            outputs: vec![ApiOutput { amount: Some(900) }],
        };
        assert_eq!(compute_fee_from_api_tx(&tx), Some(100));
    }

    #[test]
    fn compute_fee_coinbase_returns_none() {
        let tx = ApiTransaction { transaction_id: None,
            inputs: vec![],
            outputs: vec![ApiOutput { amount: Some(50_000_000_000) }],
        };
        assert_eq!(compute_fee_from_api_tx(&tx), None);
    }

    #[test]
    fn compute_fee_outputs_exceed_inputs_returns_none() {
        // Invalid on real chain — prefer None over inserting a wraparound.
        let tx = ApiTransaction { transaction_id: None,
            inputs: vec![ApiInput {
                previous_outpoint_address: None,
                previous_outpoint_amount: Some(100),
            }],
            outputs: vec![ApiOutput { amount: Some(200) }],
        };
        assert_eq!(compute_fee_from_api_tx(&tx), None);
    }

    #[test]
    fn compute_fee_zero_fee_is_valid() {
        let tx = ApiTransaction { transaction_id: None,
            inputs: vec![ApiInput {
                previous_outpoint_address: None,
                previous_outpoint_amount: Some(1000),
            }],
            outputs: vec![ApiOutput { amount: Some(1000) }],
        };
        assert_eq!(compute_fee_from_api_tx(&tx), Some(0));
    }

    // ---------- batch search (POST /transactions/search) ----------

    #[test]
    fn builds_search_url() {
        assert_eq!(
            build_search_url("https://api.kaspa.org"),
            "https://api.kaspa.org/transactions/search?resolve_previous_outpoints=light"
        );
        assert_eq!(
            build_search_url("https://api.kaspa.org/"),
            "https://api.kaspa.org/transactions/search?resolve_previous_outpoints=light"
        );
    }

    #[test]
    fn builds_search_body_hex_encodes_txids() {
        let body = build_search_body(&[vec![0x97, 0xb1], vec![0xaa, 0xbb]]);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed["transactionIds"],
            serde_json::json!(["97b1", "aabb"])
        );
    }

    #[tokio::test]
    async fn fetch_batch_keys_results_by_txid_regardless_of_order() {
        // Response deliberately in REVERSE order of the request, with full
        // 32-byte txids. Both must key correctly.
        let body = br#"[
            {
                "transaction_id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "inputs": [ {"previous_outpoint_address": "kaspa:b", "previous_outpoint_amount": 200} ],
                "outputs": [ {"amount": 150} ]
            },
            {
                "transaction_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "inputs": [ {"previous_outpoint_address": "kaspa:a", "previous_outpoint_amount": 1000} ],
                "outputs": [ {"amount": 900} ]
            }
        ]"#;
        let addr = spawn_canned_http_server(body, "200 OK").await;
        let url = format!("http://{addr}/transactions/search");
        let client = Client::builder().build().unwrap();
        let tx_a = vec![0xaa; 32];
        let tx_b = vec![0xbb; 32];
        let map = fetch_enrichment_batch(
            &client,
            &url,
            &[tx_a.clone(), tx_b.clone()],
            RetryPolicy::default_polite(),
        )
        .await
        .unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map[&tx_a].senders, vec!["kaspa:a".to_string()]);
        assert_eq!(map[&tx_a].fee_sompi, Some(100));
        assert_eq!(map[&tx_b].senders, vec!["kaspa:b".to_string()]);
        assert_eq!(map[&tx_b].fee_sompi, Some(50));
    }

    #[tokio::test]
    async fn fetch_batch_missing_txs_absent_from_map() {
        // Requested 2, API knows only 1 — the other is simply absent, NOT
        // an error. Caller counts it as a miss and moves on (cursor design
        // guarantees we don't spin on it).
        let body = br#"[
            {
                "transaction_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "inputs": [ {"previous_outpoint_amount": 10} ],
                "outputs": [ {"amount": 8} ]
            }
        ]"#;
        let addr = spawn_canned_http_server(body, "200 OK").await;
        let url = format!("http://{addr}/transactions/search");
        let client = Client::builder().build().unwrap();
        let known = vec![0xaa; 32];
        let unknown = vec![0xcc; 32];
        let map = fetch_enrichment_batch(
            &client,
            &url,
            &[known.clone(), unknown.clone()],
            RetryPolicy::default_polite(),
        )
        .await
        .unwrap();
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&known));
        assert!(!map.contains_key(&unknown));
    }

    #[tokio::test]
    async fn fetch_batch_retries_transient_503_then_succeeds() {
        let success: &[u8] = br#"[
            {
                "transaction_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "inputs": [ {"previous_outpoint_amount": 7} ],
                "outputs": []
            }
        ]"#;
        let addr = spawn_n_failures_then_success(2, success).await;
        let url = format!("http://{addr}/transactions/search");
        let client = Client::builder().build().unwrap();
        let policy = RetryPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        };
        let id = vec![0xaa; 32];
        let map = fetch_enrichment_batch(&client, &url, &[id.clone()], policy)
            .await
            .expect("recovers from 2 503s");
        assert_eq!(map[&id].fee_sompi, Some(7));
    }

    #[tokio::test]
    async fn fetch_batch_permanent_400_fails_fast() {
        let addr = spawn_canned_http_server(b"bad request", "400 Bad Request").await;
        let url = format!("http://{addr}/transactions/search");
        let client = Client::builder().build().unwrap();
        let policy = RetryPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_secs(60), // observed if erroneously retried
            max_backoff: Duration::from_secs(60),
        };
        let start = std::time::Instant::now();
        let result =
            fetch_enrichment_batch(&client, &url, &[vec![0xaa; 32]], policy).await;
        assert!(result.is_err());
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "400 must fail immediately without retry sleep"
        );
    }

    #[tokio::test]
    async fn fetch_batch_skips_entries_with_missing_or_garbage_txid() {
        // Entries without transaction_id or with non-hex ids must be skipped
        // silently, not poison the whole batch.
        let body = br#"[
            { "inputs": [ {"previous_outpoint_amount": 1} ], "outputs": [] },
            { "transaction_id": "zzzz-not-hex", "inputs": [], "outputs": [] },
            {
                "transaction_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "inputs": [ {"previous_outpoint_amount": 5} ],
                "outputs": [ {"amount": 2} ]
            }
        ]"#;
        let addr = spawn_canned_http_server(body, "200 OK").await;
        let url = format!("http://{addr}/transactions/search");
        let client = Client::builder().build().unwrap();
        let id = vec![0xaa; 32];
        let map = fetch_enrichment_batch(&client, &url, &[id.clone()], RetryPolicy::default_polite())
            .await
            .unwrap();
        assert_eq!(map.len(), 1, "only the well-formed entry survives");
        assert_eq!(map[&id].fee_sompi, Some(3));
    }

    // ---------- retry policy / status classification ----------

    #[test]
    fn retry_policy_doubles_delay_per_attempt() {
        let p = RetryPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(60),
        };
        assert_eq!(p.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(p.delay_for_attempt(1), Duration::from_millis(200));
        assert_eq!(p.delay_for_attempt(2), Duration::from_millis(400));
        assert_eq!(p.delay_for_attempt(3), Duration::from_millis(800));
    }

    #[test]
    fn retry_policy_clamps_to_max_backoff() {
        let p = RetryPolicy {
            max_attempts: 100,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(5),
        };
        // 2^10 = 1024s would way exceed max_backoff. Must clamp to 5s.
        assert_eq!(p.delay_for_attempt(10), Duration::from_secs(5));
        assert_eq!(p.delay_for_attempt(50), Duration::from_secs(5));
    }

    #[test]
    fn retry_policy_default_is_polite() {
        let p = RetryPolicy::default_polite();
        // Total worst-case wait across all attempts should be reasonable.
        // 500ms + 1s + 2s + 4s = 7.5s for 4 attempts. Under 30s ceiling.
        let total: Duration = (0..p.max_attempts).map(|n| p.delay_for_attempt(n)).sum();
        assert!(total < Duration::from_secs(30), "{total:?}");
        assert!(p.max_attempts >= 3, "should retry at least 3 times");
    }

    #[test]
    fn retriable_status_classifies_correctly() {
        // Server errors → retry
        assert!(is_retriable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_retriable_status(StatusCode::BAD_GATEWAY));
        assert!(is_retriable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_retriable_status(StatusCode::GATEWAY_TIMEOUT));
        // Rate-limit + request timeout → retry
        assert!(is_retriable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retriable_status(StatusCode::REQUEST_TIMEOUT));
        // 4xx other than the above → permanent
        assert!(!is_retriable_status(StatusCode::NOT_FOUND));
        assert!(!is_retriable_status(StatusCode::BAD_REQUEST));
        assert!(!is_retriable_status(StatusCode::UNAUTHORIZED));
        assert!(!is_retriable_status(StatusCode::FORBIDDEN));
        assert!(!is_retriable_status(StatusCode::UNPROCESSABLE_ENTITY));
        // 2xx and 3xx are not "retriable" because they're not failures
        assert!(!is_retriable_status(StatusCode::OK));
        assert!(!is_retriable_status(StatusCode::CREATED));
        assert!(!is_retriable_status(StatusCode::MOVED_PERMANENTLY));
    }

    /// HTTP server that returns the FIRST `n` responses as 503, then a 200.
    /// Used to verify the retry loop actually retries and eventually succeeds.
    async fn spawn_n_failures_then_success(
        n_failures: usize,
        success_body: &'static [u8],
    ) -> std::net::SocketAddr {
        use tokio::io::AsyncReadExt as _;
        use tokio::io::AsyncWriteExt as _;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let counter = counter.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let attempt = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let (status, body): (&str, &[u8]) = if attempt < n_failures {
                        ("503 Service Unavailable", b"transient")
                    } else {
                        ("200 OK", success_body)
                    };
                    let mut resp = Vec::new();
                    let _ = write!(
                        resp,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    resp.extend_from_slice(body);
                    let _ = sock.write_all(&resp).await;
                    let _ = sock.flush().await;
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn fetch_with_retry_recovers_from_transient_503() {
        let success: &[u8] = br#"{ "inputs": [ {"previous_outpoint_address":"kaspa:ok","previous_outpoint_amount":7} ] }"#;
        let addr = spawn_n_failures_then_success(2, success).await;
        let url = format!("http://{addr}/transactions/foo");
        let client = Client::builder().build().unwrap();
        let policy = RetryPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(1), // fast for tests
            max_backoff: Duration::from_millis(10),
        };
        let txid = vec![0x97, 0xb1];
        let e = fetch_enrichment_with_retry(&client, &url, &txid, policy)
            .await
            .expect("should recover from 2 503s");
        assert_eq!(e.senders, vec!["kaspa:ok".to_string()]);
        assert_eq!(e.sender_amounts, vec![7]);
    }

    #[tokio::test]
    async fn fetch_with_retry_gives_up_after_max_attempts() {
        // Always 503: max_attempts=3 should give up and return Err.
        let addr = spawn_n_failures_then_success(usize::MAX, b"unreachable").await;
        let url = format!("http://{addr}/transactions/foo");
        let client = Client::builder().build().unwrap();
        let policy = RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        };
        let txid = vec![0x97, 0xb1];
        let result = fetch_enrichment_with_retry(&client, &url, &txid, policy).await;
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("HTTP 503"), "expected HTTP 503 in: {msg}");
    }

    #[tokio::test]
    async fn fetch_with_retry_does_not_retry_404() {
        // 404 is permanent — must NOT retry.
        let body: &[u8] = b"not found";
        let addr = spawn_canned_http_server(body, "404 Not Found").await;
        let url = format!("http://{addr}/transactions/foo");
        let client = Client::builder().build().unwrap();
        // Use a policy with a long backoff so any erroneous retry would
        // make the test slow — failing fast proves no retry happened.
        let policy = RetryPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_secs(60), // would be observed if retried
            max_backoff: Duration::from_secs(60),
        };
        let start = std::time::Instant::now();
        let txid = vec![0x97, 0xb1];
        let result = fetch_enrichment_with_retry(&client, &url, &txid, policy).await;
        let elapsed = start.elapsed();
        assert!(result.is_err());
        assert!(
            elapsed < Duration::from_secs(5),
            "404 should fail immediately without retry sleep, took {elapsed:?}"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("HTTP 404"), "{msg}");
    }
}
