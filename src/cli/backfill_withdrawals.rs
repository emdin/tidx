use anyhow::{Result, anyhow};
use clap::Args as ClapArgs;
use std::path::PathBuf;
use tracing::info;

use tidx::config::Config;
use tidx::db;
use tidx::sync::ch_sink::ClickHouseSink;
use tidx::sync::decoder::decode_withdrawals;
use tidx::sync::fetcher::RpcClient;
use tidx::sync::sink::SinkSet;

#[derive(ClapArgs)]
pub struct Args {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    pub config: PathBuf,

    /// Chain ID (uses first chain if not specified)
    #[arg(long)]
    pub chain_id: Option<u64>,

    /// First L2 block number to scan
    #[arg(long)]
    pub from: u64,

    /// Last L2 block number to scan, inclusive
    #[arg(long)]
    pub to: u64,

    /// Number of blocks per RPC batch
    #[arg(long, default_value = "500")]
    pub batch_size: u64,
}

pub async fn run(args: Args) -> Result<()> {
    if args.from > args.to {
        return Err(anyhow!(
            "--from ({}) must be <= --to ({})",
            args.from,
            args.to
        ));
    }
    if args.batch_size == 0 {
        return Err(anyhow!("--batch-size must be greater than zero"));
    }

    let config = Config::load(&args.config)?;
    let chain = if let Some(id) = args.chain_id {
        config
            .chains
            .iter()
            .find(|c| c.chain_id == id)
            .ok_or_else(|| anyhow!("Chain ID {} not found in config", id))?
    } else {
        config
            .chains
            .first()
            .ok_or_else(|| anyhow!("No chains configured"))?
    };

    let pg_url = chain.resolved_pg_url()?;
    let pool = db::create_pool(&pg_url).await?;

    let mut sinks = SinkSet::new(pool);
    if let Some(ch_config) = &chain.clickhouse {
        if ch_config.enabled {
            let database = ch_config
                .database
                .clone()
                .unwrap_or_else(|| format!("tidx_{}", chain.chain_id));
            let password = ch_config.resolved_password()?;
            let ch_sink = ClickHouseSink::new(
                &ch_config.url,
                &database,
                ch_config.user.as_deref(),
                password.as_deref(),
            )?;
            ch_sink.ensure_schema().await?;
            sinks = sinks.with_clickhouse(ch_sink);
        }
    }

    let rpc = RpcClient::new(&chain.rpc_url);
    let mut current = args.from;
    let mut scanned_blocks = 0_u64;
    let mut written_rows = 0_u64;

    info!(
        chain = %chain.name,
        chain_id = chain.chain_id,
        from = args.from,
        to = args.to,
        batch_size = args.batch_size,
        "Starting L2 withdrawal backfill"
    );

    while current <= args.to {
        let batch_to = (current + args.batch_size - 1).min(args.to);
        let blocks = rpc.get_blocks_batch(current..=batch_to).await?;
        let withdrawals: Vec<_> = blocks.iter().flat_map(decode_withdrawals).collect();

        if !withdrawals.is_empty() {
            sinks.write_l2_withdrawals(&withdrawals).await?;
            written_rows += withdrawals.len() as u64;
        }

        scanned_blocks += batch_to - current + 1;
        info!(
            from = current,
            to = batch_to,
            rows = withdrawals.len(),
            scanned_blocks,
            written_rows,
            "Backfilled L2 withdrawal batch"
        );

        if batch_to == u64::MAX {
            break;
        }
        current = batch_to + 1;
    }

    info!(
        scanned_blocks,
        written_rows, "L2 withdrawal backfill complete"
    );
    Ok(())
}
