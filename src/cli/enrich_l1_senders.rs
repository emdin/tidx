//! `tidx enrich-l1-senders` — populate the `l1_senders` /
//! `l1_sender_amounts_sompi` / `l1_enriched_at` columns on
//! `kaspa_l2_submissions` and `kaspa_entries` by asking api.kaspa.org for
//! each Kaspa tx's resolved previous-outpoint addresses.
//!
//! Thin CLI shim around `tidx::kaspa::enrichment::enrich_table`. The actual
//! logic lives in the library crate so integration tests in `tests/` can
//! exercise it end-to-end against an ephemeral postgres + a fake HTTP
//! server.

use anyhow::{Result, anyhow};
use clap::Args as ClapArgs;
use reqwest::Client;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};

use tidx::config::Config;
use tidx::db;
use tidx::kaspa::enrichment::enrich_table;

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

    /// Rows fetched from PG per work batch.
    #[arg(long, default_value = "200")]
    pub batch_size: usize,

    /// Stop after enriching this many rows (per table).
    #[arg(long)]
    pub max_rows: Option<usize>,

    /// Don't write; only count what would be enriched (skips DB pool entirely).
    #[arg(long)]
    pub dry_run: bool,
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
        .build()?;

    if args.dry_run {
        warn!("--dry-run skips DB entirely; this CLI is mostly useful in non-dry mode. exiting.");
        return Ok(());
    }

    let pg_url = chain.resolved_pg_url()?;
    let pool = db::create_pool(&pg_url).await?;

    info!(rest_base = %args.rest_base, table = %args.table,
        concurrency = args.concurrency, batch_size = args.batch_size,
        "enrich-l1-senders starting");

    let do_subs = args.table == "both" || args.table == "submissions";
    let do_entries = args.table == "both" || args.table == "entries";

    if do_subs {
        enrich_table(
            &pool,
            &client,
            &args.rest_base,
            "kaspa_l2_submissions",
            args.concurrency,
            args.batch_size,
            args.max_rows,
        )
        .await?;
    }
    if do_entries {
        enrich_table(
            &pool,
            &client,
            &args.rest_base,
            "kaspa_entries",
            args.concurrency,
            args.batch_size,
            args.max_rows,
        )
        .await?;
    }
    Ok(())
}
