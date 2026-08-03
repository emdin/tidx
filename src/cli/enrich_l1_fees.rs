//! `tidx enrich-l1-fees` — thin CLI shim around
//! [`tidx::kaspa::fee_enrichment::enrich_table`]. Local-only: uses the
//! kaspad reachable at `--kaspad-rpc` plus PG for the `kaspa_tx_index`
//! lookup. No external API dependency.

use anyhow::{Result, anyhow};
use clap::Args as ClapArgs;
use std::path::PathBuf;
use tracing::{info, warn};

use tidx::config::Config;
use tidx::db;
use tidx::kaspa::client::connect_borsh_wrpc;
use tidx::kaspa::fee::{FeeResolver, KaspadBlockFetcher, PgTxLocator};
use tidx::kaspa::fee_enrichment;

#[derive(ClapArgs)]
pub struct Args {
    /// Path to config file (used for DB connection only).
    #[arg(short, long, default_value = "config.toml")]
    pub config: PathBuf,

    /// Chain ID (uses first chain if not specified).
    #[arg(long)]
    pub chain_id: Option<u64>,

    /// Kaspa wRPC URL (Borsh). For local enrichment this should be the
    /// loopback kaspad — `ws://127.0.0.1:17110` from inside the tidx
    /// container maps to the `kaspad-mainnet` sidecar.
    #[arg(long, default_value = "ws://127.0.0.1:17110")]
    pub kaspad_rpc: String,

    /// Which table(s) to enrich.
    #[arg(long, default_value = "both",
        value_parser = clap::builder::PossibleValuesParser::new(["both", "submissions", "entries"]))]
    pub table: String,

    /// Rows fetched from PG per work batch.
    #[arg(long, default_value = "200")]
    pub batch_size: usize,

    /// Stop after enriching this many rows (per table).
    #[arg(long)]
    pub max_rows: Option<usize>,

    /// Don't touch PG; used for CI wiring checks.
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

    if args.dry_run {
        warn!("--dry-run: skipping DB pool + kaspad connect. Exiting.");
        return Ok(());
    }

    let pg_url = chain.resolved_pg_url()?;
    let pool = db::create_pool(&pg_url).await?;

    info!(rpc = %args.kaspad_rpc, "connecting to kaspad wRPC");
    let kaspad = connect_borsh_wrpc(&args.kaspad_rpc).await?;

    let locator = PgTxLocator::new(&pool);
    let fetcher = KaspadBlockFetcher::new(&kaspad);
    let resolver = FeeResolver::new(&locator, &fetcher);

    let do_subs = args.table == "both" || args.table == "submissions";
    let do_entries = args.table == "both" || args.table == "entries";

    info!(table = %args.table, batch = args.batch_size, "enrich-l1-fees starting");

    if do_subs {
        fee_enrichment::enrich_table(
            &pool,
            &resolver,
            "kaspa_l2_submissions",
            args.batch_size,
            args.max_rows,
        )
        .await?;
    }
    if do_entries {
        fee_enrichment::enrich_table(
            &pool,
            &resolver,
            "kaspa_entries",
            args.batch_size,
            args.max_rows,
        )
        .await?;
    }
    Ok(())
}
