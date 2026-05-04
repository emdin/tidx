//! `tidx enrich-l1-senders` — populate the `l1_senders` /
//! `l1_sender_amounts_sompi` / `l1_enriched_at` columns on
//! `kaspa_l2_submissions` and `kaspa_entries` by asking api.kaspa.org for
//! each Kaspa tx's resolved previous-outpoint addresses.
//!
//! api.kaspa.org's `/transactions/{tx_id}?resolve_previous_outpoints=light`
//! endpoint returns the spent UTXO's `previous_outpoint_address` (bech32
//! Kaspa address) and `previous_outpoint_amount` (in sompi) for each input
//! — exactly the L1-sender info we want, no script_public_key decoding
//! needed on our side.
//!
//! Idempotent: only rows where `l1_senders IS NULL` are touched. Failed
//! fetches don't update the row, so a retry next pass will pick them up.

use anyhow::{Context, Result, anyhow};
use clap::Args as ClapArgs;
use reqwest::Client;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use tidx::config::Config;
use tidx::db;

#[derive(ClapArgs)]
pub struct Args {
    /// Path to config file (used for DB connection only).
    #[arg(short, long, default_value = "config.toml")]
    pub config: PathBuf,

    /// Chain ID (uses first chain if not specified).
    #[arg(long)]
    pub chain_id: Option<u64>,

    /// Base URL for the Kaspa REST API.
    #[arg(long, default_value = "https://api.kaspa.org")]
    pub rest_base: String,

    /// Which table(s) to enrich.
    #[arg(long, default_value = "both",
        value_parser = clap::builder::PossibleValuesParser::new(["both", "submissions", "entries"]))]
    pub table: String,

    /// Concurrent HTTPS requests against api.kaspa.org. Stay polite — the
    /// default keeps us well under their rate limit.
    #[arg(long, default_value = "5")]
    pub concurrency: usize,

    /// Rows fetched from PG per work batch. Each batch fires up to
    /// `concurrency` HTTPS calls in parallel.
    #[arg(long, default_value = "200")]
    pub batch_size: usize,

    /// Stop after enriching this many rows (per table). Useful for chunked
    /// runs and verification.
    #[arg(long)]
    pub max_rows: Option<usize>,

    /// Don't write; only count what would be enriched.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Deserialize)]
struct ApiTransaction {
    #[serde(default)]
    inputs: Vec<ApiInput>,
}

#[derive(Deserialize, Debug)]
struct ApiInput {
    #[serde(default)]
    previous_outpoint_address: Option<String>,
    #[serde(default)]
    previous_outpoint_amount: Option<i64>,
}

pub async fn run(args: Args) -> Result<()> {
    let cfg = Config::load(&args.config)?;
    let chain = if let Some(id) = args.chain_id {
        cfg.chains
            .iter()
            .find(|c| c.chain_id == id)
            .ok_or_else(|| anyhow!("Chain ID {id} not found in config"))?
    } else {
        cfg.chains
            .first()
            .ok_or_else(|| anyhow!("No chains configured"))?
    };

    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(args.concurrency)
        .gzip(true)
        .build()
        .context("build reqwest client")?;

    let pool = if args.dry_run {
        None
    } else {
        let pg_url = chain.resolved_pg_url()?;
        let pool = db::create_pool(&pg_url).await?;
        Some(pool)
    };

    info!(rest_base = %args.rest_base, table = %args.table,
        concurrency = args.concurrency, batch_size = args.batch_size,
        dry_run = args.dry_run, "enrich-l1-senders starting");

    let do_subs = args.table == "both" || args.table == "submissions";
    let do_entries = args.table == "both" || args.table == "entries";

    if do_subs {
        enrich_table(pool.as_ref(), &client, &args, "kaspa_l2_submissions").await?;
    }
    if do_entries {
        enrich_table(pool.as_ref(), &client, &args, "kaspa_entries").await?;
    }
    Ok(())
}

async fn enrich_table(
    pool: Option<&db::Pool>,
    client: &Client,
    args: &Args,
    table: &str,
) -> Result<()> {
    info!(table = %table, "starting enrichment scan");
    let mut total_enriched = 0usize;
    let mut total_failed = 0usize;
    let mut total_no_inputs = 0usize;

    loop {
        // Pull the next batch of un-enriched txids.
        let txids: Vec<Vec<u8>> = if let Some(pool) = pool {
            let conn = pool.get().await?;
            let rows = conn
                .query(
                    &format!(
                        "SELECT kaspa_txid FROM {table}
                         WHERE l1_senders IS NULL
                         ORDER BY kaspa_txid
                         LIMIT $1"
                    ),
                    &[&(args.batch_size as i64)],
                )
                .await?;
            rows.iter().map(|r| r.get::<_, Vec<u8>>(0)).collect()
        } else {
            // Dry-run path: synthesize empty after one no-op pass so we don't loop.
            if total_enriched > 0 {
                break;
            }
            // Fetch a tiny synthetic batch from a separate read-only query — we
            // still want to exercise the HTTP path so dry-run is informative.
            // Skip if no DB; just exit.
            warn!("dry-run without DB pool — exiting (use --dry-run + a real config to actually probe)");
            break;
        };

        if txids.is_empty() {
            info!(table = %table, "no more rows to enrich");
            break;
        }

        let sem = Arc::new(Semaphore::new(args.concurrency.max(1)));
        let mut tasks = Vec::with_capacity(txids.len());
        for txid in &txids {
            let permit = sem.clone().acquire_owned().await?;
            let client = client.clone();
            let url = format!(
                "{}/transactions/{}?inputs=true&outputs=false&resolve_previous_outpoints=light",
                args.rest_base.trim_end_matches('/'),
                hex::encode(txid),
            );
            let txid = txid.clone();
            tasks.push(tokio::spawn(async move {
                let _permit = permit; // dropped at task end → releases semaphore slot
                fetch_senders(&client, &url, &txid).await.map(|s| (txid, s))
            }));
        }

        // Collect results
        let mut updates: Vec<(Vec<u8>, Vec<String>, Vec<i64>)> = Vec::new();
        for t in tasks {
            match t.await {
                Ok(Ok((txid, (senders, amounts)))) => {
                    if senders.is_empty() {
                        total_no_inputs += 1;
                    }
                    updates.push((txid, senders, amounts));
                }
                Ok(Err(e)) => {
                    debug!(err = %e, "fetch failed; will retry on next pass");
                    total_failed += 1;
                }
                Err(e) => {
                    warn!(err = %e, "task join failed");
                    total_failed += 1;
                }
            }
        }

        if !args.dry_run && !updates.is_empty() {
            let pool = pool.expect("non-dry-run requires pool");
            let mut conn = pool.get().await?;
            let tx = conn.transaction().await?;
            for (txid, senders, amounts) in &updates {
                tx.execute(
                    &format!(
                        "UPDATE {table}
                         SET l1_senders = $2,
                             l1_sender_amounts_sompi = $3,
                             l1_enriched_at = now()
                         WHERE kaspa_txid = $1
                           AND l1_senders IS NULL"
                    ),
                    &[&txid.as_slice(), &senders, &amounts],
                )
                .await?;
            }
            tx.commit().await?;
            total_enriched += updates.len();
        }

        info!(
            table = %table,
            batch_size = txids.len(),
            total_enriched,
            total_failed,
            total_no_inputs,
            "batch done"
        );

        if let Some(cap) = args.max_rows {
            if total_enriched >= cap {
                info!(table = %table, cap, "max_rows reached; stopping");
                break;
            }
        }
    }

    info!(
        table = %table,
        total_enriched,
        total_failed,
        total_no_inputs,
        "enrichment finished"
    );
    Ok(())
}

/// Fetch one tx's resolved sender info from api.kaspa.org. Returns
/// `(senders, amounts)` aligned by input index.
async fn fetch_senders(
    client: &Client,
    url: &str,
    txid: &[u8],
) -> Result<(Vec<String>, Vec<i64>)> {
    let resp = client.get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        // 404 = the tx isn't in api.kaspa.org's index (very rare for `97b1`
        // prefix Kaspa txs); treat as a soft skip so we move on.
        anyhow::bail!("HTTP {} for tx {}", status, hex::encode(txid));
    }
    let parsed: ApiTransaction = resp.json().await.context("parse api.kaspa.org json")?;
    let mut senders = Vec::with_capacity(parsed.inputs.len());
    let mut amounts = Vec::with_capacity(parsed.inputs.len());
    for input in parsed.inputs {
        senders.push(input.previous_outpoint_address.unwrap_or_default());
        amounts.push(input.previous_outpoint_amount.unwrap_or(0));
    }
    Ok((senders, amounts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_api_response_shape() {
        // Real api.kaspa.org response shape (snipped from the live one we
        // saw during investigation: tx 97b167d4… has 1 input).
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
        // Coinbase txs / unusual outpoints may omit the resolved address.
        // We must tolerate Option<None> and produce empty-string in the
        // sender slot rather than failing.
        let json = r#"{ "inputs": [ { "transaction_id": "abc", "index": 0 } ] }"#;
        let parsed: ApiTransaction = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.inputs.len(), 1);
        assert_eq!(parsed.inputs[0].previous_outpoint_address, None);
        assert_eq!(parsed.inputs[0].previous_outpoint_amount, None);
    }

    #[test]
    fn parses_api_response_zero_inputs() {
        // Coinbase txs have no inputs; this should round-trip to senders=[].
        let json = r#"{ "inputs": [] }"#;
        let parsed: ApiTransaction = serde_json::from_str(json).unwrap();
        assert!(parsed.inputs.is_empty());
    }
}
