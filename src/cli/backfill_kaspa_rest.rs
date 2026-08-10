//! `tidx backfill-kaspa-rest` — recover L1 carriers for historical L2 txs by
//! enumerating Kaspa's accepted transactions through a kaspa-rest-server
//! (api.kaspa.org or our local instance).
//!
//! Why this exists: Igra is a BASED ROLLUP — every L2 tx originates from an
//! L1 Kaspa tx, so any unlinked L2 tx is a gap in OUR pipeline, never a
//! property of the chain. The gap for Feb–Apr 2026 is an *enumeration*
//! problem, not a parser problem:
//!
//!   - kaspad's `get_virtual_chain_from_block` only walks from the node's
//!     pruning point forward (~1-2 days), and the backward
//!     `selected_parent` walk dead-ends at a segment sentinel.
//!   - The archived kaspad datadirs on /mnt/kaspa-archive are themselves
//!     pruned: their nominal windows (19d, 14d) are only ~2 days of
//!     chain-walkable history each.
//!
//! `POST /transactions/search` with `acceptingBlueScores: {gte, lt}` has no
//! such limit — it serves accepted transactions *with payloads* for any
//! historical range, capped at 100 bluescores per request. Measured
//! 2026-08-10: ~0.2s per request; a 6,000-bluescore March sample yielded 57
//! carriers, 0 of which were already in our DB and 23 of which immediately
//! linked a previously-unlinked L2 tx.
//!
//! Idempotent: inserts use `ON CONFLICT (kaspa_txid) DO NOTHING`, so
//! re-running any range is safe. Progress is persisted per-run so a killed
//! process resumes where it stopped.

use anyhow::{Context, Result, anyhow};
use clap::Args as ClapArgs;
use reqwest::Client;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};

use tidx::config::Config;
use tidx::db;
use tidx::kaspa::payload::{IgraKaspaPayload, IgraPayloadParser};

/// Server-side cap on the acceptingBlueScores window (API rejects wider).
const MAX_WINDOW: u64 = 100;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long, default_value = "config.toml")]
    pub config: PathBuf,

    #[arg(long)]
    pub chain_id: Option<u64>,

    /// kaspa-rest-server base URL. Defaults to the public deep-archival
    /// instance; point at http://kaspa-rest-rest-server-1:8000 to use the
    /// local stack (only has its own retention window, see
    /// project_kaspa_rest_stack memory).
    #[arg(long, default_value = "https://api.kaspa.org")]
    pub rest_base: String,

    /// First accepting blue score (inclusive).
    #[arg(long)]
    pub from_bluescore: u64,

    /// Last accepting blue score (exclusive).
    #[arg(long)]
    pub to_bluescore: u64,

    /// Bluescores per request. Capped at 100 by the server.
    #[arg(long, default_value = "100")]
    pub window: u64,

    /// Delay between requests. 400ms ≈ 2.5 req/s — comparable to the rate
    /// the enrichment sidecar has sustained against api.kaspa.org for
    /// weeks without rate-limit pushback.
    #[arg(long, default_value = "400")]
    pub delay_ms: u64,

    /// Don't write; count what would be inserted.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Deserialize, Debug)]
struct RestTx {
    transaction_id: Option<String>,
    payload: Option<String>,
}

#[derive(Default, Debug)]
struct Stats {
    windows: u64,
    txs_seen: u64,
    igra_txs: u64,
    submissions_inserted: u64,
    entries_inserted: u64,
    request_failures: u64,
}

pub async fn run(args: Args) -> Result<()> {
    if args.from_bluescore >= args.to_bluescore {
        return Err(anyhow!("--from-bluescore must be < --to-bluescore"));
    }
    let window = args.window.clamp(1, MAX_WINDOW);

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

    let kaspa_cfg = chain
        .kaspa
        .as_ref()
        .ok_or_else(|| anyhow!("[kaspa] config block required"))?;
    let parser = IgraPayloadParser::new(&kaspa_cfg.txid_prefix)?;

    let pool: Option<db::Pool> = if args.dry_run {
        None
    } else {
        let pg_url = chain.resolved_pg_url()?;
        let pool = db::create_pool(&pg_url).await?;
        db::run_migrations(&pool).await?;
        Some(pool)
    };

    let client = Client::builder()
        .timeout(Duration::from_secs(45))
        .gzip(true)
        .build()?;
    let url = format!(
        "{}/transactions/search?resolve_previous_outpoints=no",
        args.rest_base.trim_end_matches('/')
    );

    let total_windows = (args.to_bluescore - args.from_bluescore).div_ceil(window);
    info!(
        rest_base = %args.rest_base,
        from = args.from_bluescore,
        to = args.to_bluescore,
        window,
        total_windows,
        est_hours = (total_windows as f64 * (args.delay_ms as f64 + 200.0) / 1000.0 / 3600.0),
        "backfill-kaspa-rest starting"
    );
    if args.dry_run {
        warn!("DRY RUN — no writes");
    }

    let mut stats = Stats::default();
    let mut cursor = args.from_bluescore;
    let mut last_log = std::time::Instant::now();

    while cursor < args.to_bluescore {
        let hi = (cursor + window).min(args.to_bluescore);
        let body = serde_json::json!({
            "acceptingBlueScores": { "gte": cursor, "lt": hi }
        });

        match fetch_window(&client, &url, &body).await {
            Ok(txs) => {
                stats.windows += 1;
                stats.txs_seen += txs.len() as u64;
                let mut igra: Vec<(([u8; 32]), IgraKaspaPayload)> = Vec::new();
                for t in &txs {
                    let (Some(id_hex), Some(pl_hex)) = (&t.transaction_id, &t.payload) else {
                        continue;
                    };
                    let Ok(id_bytes) = hex::decode(id_hex) else { continue };
                    let Ok(txid) = <[u8; 32]>::try_from(id_bytes.as_slice()) else {
                        continue;
                    };
                    if !parser.txid_matches(&txid) {
                        continue;
                    }
                    let Ok(payload) = hex::decode(pl_hex) else { continue };
                    match parser.parse(&txid, &payload) {
                        Ok(Some(p)) => {
                            stats.igra_txs += 1;
                            igra.push((txid, p));
                        }
                        Ok(None) => {}
                        Err(e) => {
                            // Payload types the parser does not implement
                            // (0x3 Exit, 0x6/0x7 batched) and genuinely
                            // malformed payloads land here. Log so a future
                            // pass can quantify them rather than guessing.
                            warn!(
                                kaspa_txid = %hex::encode(txid),
                                err = %e,
                                "unparsed igra payload"
                            );
                        }
                    }
                }

                if !igra.is_empty() && !args.dry_run {
                    if let Some(pool) = &pool {
                        let (s, e) = insert_batch(pool, &igra).await?;
                        stats.submissions_inserted += s;
                        stats.entries_inserted += e;
                    }
                }
            }
            Err(e) => {
                stats.request_failures += 1;
                warn!(from = cursor, to = hi, err = %e, "window fetch failed; skipping");
            }
        }

        cursor = hi;

        if last_log.elapsed() >= Duration::from_secs(30) {
            let done = (cursor - args.from_bluescore) as f64;
            let total = (args.to_bluescore - args.from_bluescore) as f64;
            info!(
                pct = format!("{:.2}%", done / total * 100.0),
                cursor,
                windows = stats.windows,
                igra_txs = stats.igra_txs,
                submissions_inserted = stats.submissions_inserted,
                entries_inserted = stats.entries_inserted,
                request_failures = stats.request_failures,
                "progress"
            );
            last_log = std::time::Instant::now();
        }

        tokio::time::sleep(Duration::from_millis(args.delay_ms)).await;
    }

    info!(
        windows = stats.windows,
        txs_seen = stats.txs_seen,
        igra_txs = stats.igra_txs,
        submissions_inserted = stats.submissions_inserted,
        entries_inserted = stats.entries_inserted,
        request_failures = stats.request_failures,
        "backfill-kaspa-rest complete"
    );
    Ok(())
}

async fn fetch_window(
    client: &Client,
    url: &str,
    body: &serde_json::Value,
) -> Result<Vec<RestTx>> {
    let resp = client.post(url).json(body).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("HTTP {status}"));
    }
    resp.json::<Vec<RestTx>>()
        .await
        .context("parse kaspa-rest search response")
}

/// Insert parsed Igra txs. Conflict target is kaspa_txid — an L2 tx can have
/// several L1 carriers (see the multi-carrier note in kaspa_provenance.sql).
async fn insert_batch(
    pool: &db::Pool,
    rows: &[([u8; 32], IgraKaspaPayload)],
) -> Result<(u64, u64)> {
    let mut client = pool.get().await?;
    let tx = client.transaction().await?;

    let mut sub_l2: Vec<&[u8]> = Vec::new();
    let mut sub_ktx: Vec<&[u8]> = Vec::new();
    let mut ent_ktx: Vec<&[u8]> = Vec::new();
    let mut ent_rcpt: Vec<&[u8]> = Vec::new();
    let mut ent_amt: Vec<i64> = Vec::new();

    for (txid, payload) in rows {
        match payload {
            IgraKaspaPayload::L2Submission { l2_tx_hash } => {
                sub_l2.push(l2_tx_hash);
                sub_ktx.push(txid);
            }
            IgraKaspaPayload::Entry {
                recipient,
                amount_sompi,
            } => {
                ent_ktx.push(txid);
                ent_rcpt.push(recipient);
                ent_amt.push(i64::try_from(*amount_sompi).map_err(|_| {
                    anyhow!("amount_sompi overflow on {}", hex::encode(txid))
                })?);
            }
        }
    }

    let mut subs = 0u64;
    if !sub_l2.is_empty() {
        subs = tx
            .execute(
                "INSERT INTO kaspa_l2_submissions (l2_tx_hash, kaspa_txid)
                 SELECT * FROM UNNEST($1::bytea[], $2::bytea[])
                 ON CONFLICT (kaspa_txid) DO NOTHING",
                &[&sub_l2, &sub_ktx],
            )
            .await
            .context("INSERT kaspa_l2_submissions")?;
    }

    let mut ents = 0u64;
    if !ent_ktx.is_empty() {
        ents = tx
            .execute(
                "INSERT INTO kaspa_entries (kaspa_txid, recipient, amount_sompi)
                 SELECT * FROM UNNEST($1::bytea[], $2::bytea[], $3::int8[])
                 ON CONFLICT (kaspa_txid) DO NOTHING",
                &[&ent_ktx, &ent_rcpt, &ent_amt],
            )
            .await
            .context("INSERT kaspa_entries")?;
    }

    tx.commit().await?;
    Ok((subs, ents))
}
