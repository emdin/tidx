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
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
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

/// Subset of api.kaspa.org's `/transactions/{id}` response that we care
/// about. Other fields (verbose data, signature scripts, mass, etc.) are
/// intentionally not bound — serde will ignore them.
#[derive(Deserialize, Debug, PartialEq, Eq)]
pub struct ApiTransaction {
    #[serde(default)]
    pub inputs: Vec<ApiInput>,
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
pub struct ApiInput {
    #[serde(default)]
    pub previous_outpoint_address: Option<String>,
    #[serde(default)]
    pub previous_outpoint_amount: Option<i64>,
}

/// Build the api.kaspa.org URL for resolving one tx's previous-outpoint
/// senders. Pure function so it's directly unit-testable. Trailing slashes
/// on the base URL are stripped so the path is canonical.
pub fn build_tx_url(rest_base: &str, txid: &[u8]) -> String {
    format!(
        "{}/transactions/{}?inputs=true&outputs=false&resolve_previous_outpoints=light",
        rest_base.trim_end_matches('/'),
        hex::encode(txid),
    )
}

/// Build the per-row UPDATE SQL for a given table. The table name is
/// interpolated (allowed because callers pass a known table name from a
/// CLI enum, not user-supplied free text), so this isn't a SQL-injection
/// vector. Returned as a `String` for testability.
pub fn build_update_sql(table: &str) -> String {
    format!(
        "UPDATE {table}
         SET l1_senders = $2,
             l1_sender_amounts_sompi = $3,
             l1_enriched_at = now()
         WHERE kaspa_txid = $1
           AND l1_senders IS NULL"
    )
}

/// Fetch one tx's resolved sender info from api.kaspa.org. Returns
/// `(senders, amounts)` aligned by input index. On non-2xx HTTP, returns
/// `Err` so the caller can soft-skip and retry on next pass.
///
/// Convenience wrapper using the default retry policy.
pub async fn fetch_senders(
    client: &Client,
    url: &str,
    txid: &[u8],
) -> Result<(Vec<String>, Vec<i64>)> {
    fetch_senders_with_retry(client, url, txid, RetryPolicy::default_polite()).await
}

/// Same as `fetch_senders` but with an explicit retry policy. Transient
/// errors (5xx, 408, 429, network errors) are retried with exponential
/// backoff up to `policy.max_attempts`; permanent errors (other 4xx)
/// abort immediately.
pub async fn fetch_senders_with_retry(
    client: &Client,
    url: &str,
    txid: &[u8],
    policy: RetryPolicy,
) -> Result<(Vec<String>, Vec<i64>)> {
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..policy.max_attempts {
        match try_fetch_senders_once(client, url, txid).await {
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
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("fetch_senders: out of retries with no error")))
}

/// Internal classification of fetch errors so the retry loop knows
/// which to keep trying and which to bail on immediately.
enum FetchError {
    /// 4xx (other than 408/429), JSON parse failure — won't be different next time.
    Permanent(anyhow::Error),
    /// 5xx, 408, 429, network/timeout — worth retrying.
    Transient(anyhow::Error),
}

async fn try_fetch_senders_once(
    client: &Client,
    url: &str,
    txid: &[u8],
) -> std::result::Result<(Vec<String>, Vec<i64>), FetchError> {
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
    let mut senders = Vec::with_capacity(parsed.inputs.len());
    let mut amounts = Vec::with_capacity(parsed.inputs.len());
    for input in parsed.inputs {
        senders.push(input.previous_outpoint_address.unwrap_or_default());
        amounts.push(input.previous_outpoint_amount.unwrap_or(0));
    }
    Ok((senders, amounts))
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
pub async fn enrich_table(
    pool: &db::Pool,
    client: &Client,
    rest_base: &str,
    table: &str,
    concurrency: usize,
    batch_size: usize,
    max_rows: Option<usize>,
) -> Result<EnrichmentStats> {
    info!(table = %table, "starting enrichment scan");
    let mut stats = EnrichmentStats::default();

    loop {
        // Pull the next batch of un-enriched txids.
        let conn = pool.get().await?;
        let rows = conn
            .query(
                &format!(
                    "SELECT kaspa_txid FROM {table}
                     WHERE l1_senders IS NULL
                     ORDER BY kaspa_txid
                     LIMIT $1"
                ),
                &[&(batch_size as i64)],
            )
            .await?;
        drop(conn);

        let txids: Vec<Vec<u8>> = rows.iter().map(|r| r.get::<_, Vec<u8>>(0)).collect();
        if txids.is_empty() {
            info!(table = %table, "no more rows to enrich");
            break;
        }

        // Concurrent fetches with a semaphore to limit pressure on the API.
        let sem = Arc::new(Semaphore::new(concurrency.max(1)));
        let mut tasks = Vec::with_capacity(txids.len());
        for txid in &txids {
            let permit = sem.clone().acquire_owned().await?;
            let client = client.clone();
            let url = build_tx_url(rest_base, txid);
            let txid = txid.clone();
            tasks.push(tokio::spawn(async move {
                let _permit = permit; // dropped at task end → releases semaphore slot
                fetch_senders(&client, &url, &txid).await.map(|s| (txid, s))
            }));
        }

        let mut updates: Vec<(Vec<u8>, Vec<String>, Vec<i64>)> = Vec::new();
        for t in tasks {
            match t.await {
                Ok(Ok((txid, (senders, amounts)))) => {
                    if senders.is_empty() {
                        stats.no_inputs += 1;
                    }
                    updates.push((txid, senders, amounts));
                }
                Ok(Err(e)) => {
                    debug!(err = %e, "fetch failed; will retry on next pass");
                    stats.failed += 1;
                }
                Err(e) => {
                    warn!(err = %e, "task join failed");
                    stats.failed += 1;
                }
            }
        }

        if !updates.is_empty() {
            let mut conn = pool.get().await?;
            let tx = conn.transaction().await?;
            let sql = build_update_sql(table);
            for (txid, senders, amounts) in &updates {
                tx.execute(&sql, &[&txid.as_slice(), &senders, &amounts])
                    .await?;
            }
            tx.commit().await?;
            stats.enriched += updates.len();
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
            "https://api.kaspa.org/transactions/97b167d4318621a9abb91003b2d5bd1a6f20aa638124644b356faecbb13c4f5e?inputs=true&outputs=false&resolve_previous_outpoints=light"
        );
    }

    #[test]
    fn builds_tx_url_strips_trailing_slash_on_base() {
        let txid = hex::decode("aa").unwrap();
        let url = build_tx_url("https://api.kaspa.org/", &txid);
        assert!(!url.contains("//transactions"), "double-slash: {url}");
        assert_eq!(
            url,
            "https://api.kaspa.org/transactions/aa?inputs=true&outputs=false&resolve_previous_outpoints=light"
        );
    }

    // ---------- UPDATE SQL ----------

    #[test]
    fn build_update_sql_targets_the_named_table() {
        let sql = build_update_sql("kaspa_l2_submissions");
        assert!(sql.contains("UPDATE kaspa_l2_submissions"), "{sql}");
        assert!(sql.contains("l1_senders = $2"), "{sql}");
        assert!(sql.contains("l1_sender_amounts_sompi = $3"), "{sql}");
        assert!(sql.contains("l1_enriched_at = now()"), "{sql}");
        assert!(sql.contains("WHERE kaspa_txid = $1"), "{sql}");
        // Idempotency: must not overwrite already-enriched rows on a re-run.
        assert!(sql.contains("AND l1_senders IS NULL"), "{sql}");
    }

    #[test]
    fn build_update_sql_works_for_kaspa_entries_too() {
        let sql = build_update_sql("kaspa_entries");
        assert!(sql.contains("UPDATE kaspa_entries"), "{sql}");
        assert!(sql.contains("AND l1_senders IS NULL"), "{sql}");
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
    async fn fetch_senders_round_trips_real_api_shape() {
        let body = br#"{
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
        let addr = spawn_canned_http_server(body, "200 OK").await;
        let url = format!("http://{addr}/transactions/foo");
        let client = Client::builder().build().unwrap();
        let txid = vec![0x97, 0xb1];
        let (senders, amounts) = fetch_senders(&client, &url, &txid).await.unwrap();
        assert_eq!(senders.len(), 1);
        assert_eq!(amounts.len(), 1);
        assert_eq!(
            senders[0],
            "kaspa:qq5xkhfdmm4zzwc25udlmkcg24vefhc54snklphd3slrvcrexspcg40fvxxh4"
        );
        assert_eq!(amounts[0], 10985439355);
    }

    #[tokio::test]
    async fn fetch_senders_handles_404_as_error() {
        let addr = spawn_canned_http_server(b"not found", "404 Not Found").await;
        let url = format!("http://{addr}/transactions/foo");
        let client = Client::builder().build().unwrap();
        let txid = vec![0x97, 0xb1];
        let result = fetch_senders(&client, &url, &txid).await;
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("HTTP 404"), "expected HTTP 404 in: {msg}");
    }

    #[tokio::test]
    async fn fetch_senders_handles_zero_input_response() {
        let body = br#"{ "inputs": [] }"#;
        let addr = spawn_canned_http_server(body, "200 OK").await;
        let url = format!("http://{addr}/transactions/foo");
        let client = Client::builder().build().unwrap();
        let txid = vec![0x97, 0xb1];
        let (senders, amounts) = fetch_senders(&client, &url, &txid).await.unwrap();
        assert!(senders.is_empty());
        assert!(amounts.is_empty());
    }

    #[tokio::test]
    async fn fetch_senders_substitutes_defaults_for_missing_optional_fields() {
        let body = br#"{ "inputs": [ {}, {"previous_outpoint_amount": 42} ] }"#;
        let addr = spawn_canned_http_server(body, "200 OK").await;
        let url = format!("http://{addr}/transactions/foo");
        let client = Client::builder().build().unwrap();
        let txid = vec![0x97, 0xb1];
        let (senders, amounts) = fetch_senders(&client, &url, &txid).await.unwrap();
        assert_eq!(senders, vec!["".to_string(), "".to_string()]);
        assert_eq!(amounts, vec![0i64, 42i64]);
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
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
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
        let (senders, amounts) = fetch_senders_with_retry(&client, &url, &txid, policy)
            .await
            .expect("should recover from 2 503s");
        assert_eq!(senders, vec!["kaspa:ok".to_string()]);
        assert_eq!(amounts, vec![7]);
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
        let result = fetch_senders_with_retry(&client, &url, &txid, policy).await;
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
        let result = fetch_senders_with_retry(&client, &url, &txid, policy).await;
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
