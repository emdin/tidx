use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use tracing::info;

use crate::db::{self, PartitionManager};
use crate::sync::decoder::{decode_block, decode_transaction};
use crate::sync::fetcher::RpcClient;
use crate::sync::writer::{write_block, write_txs};

#[derive(ClapArgs)]
pub struct Args {
    /// RPC endpoint URL
    #[arg(long, env = "AK47_RPC_URL")]
    pub rpc: String,

    /// Database URL
    #[arg(long, env = "AK47_DATABASE_URL")]
    pub db: String,

    #[command(subcommand)]
    pub command: SyncCommands,
}

#[derive(Subcommand)]
pub enum SyncCommands {
    /// Sync blocks forward from a range
    Forward {
        /// Start block number
        #[arg(long)]
        from: u64,

        /// End block number
        #[arg(long)]
        to: u64,

        /// Batch size for RPC requests
        #[arg(long, default_value = "100")]
        batch_size: u64,
    },
}

pub async fn run(args: Args) -> Result<()> {
    let pool = db::create_pool(&args.db).await?;
    db::run_migrations(&pool).await?;

    let rpc = RpcClient::new(&args.rpc);
    let partitions = PartitionManager::new(pool.clone());

    match args.command {
        SyncCommands::Forward {
            from,
            to,
            batch_size,
        } => {
            run_forward(&pool, &rpc, &partitions, from, to, batch_size).await?;
        }
    }

    Ok(())
}

async fn run_forward(
    pool: &db::Pool,
    rpc: &RpcClient,
    partitions: &PartitionManager,
    from: u64,
    to: u64,
    batch_size: u64,
) -> Result<()> {
    info!(from, to, batch_size, "Starting forward sync");

    let mut synced = 0u64;
    let total = to - from + 1;

    for chunk_start in (from..=to).step_by(batch_size as usize) {
        let chunk_end = (chunk_start + batch_size - 1).min(to);

        partitions.ensure_partition(chunk_end).await?;

        let blocks = rpc.get_blocks_batch(chunk_start..=chunk_end).await?;

        for block in &blocks {
            let block_row = decode_block(block);
            write_block(pool, &block_row).await?;

            let txs: Vec<_> = block
                .transactions()
                .enumerate()
                .map(|(i, tx)| decode_transaction(tx, block, i as u32))
                .collect();

            write_txs(pool, &txs).await?;
        }

        synced += blocks.len() as u64;
        let pct = (synced as f64 / total as f64) * 100.0;
        info!(
            chunk_start,
            chunk_end,
            synced,
            total,
            pct = format!("{:.1}%", pct),
            "Synced batch"
        );
    }

    info!(synced, "Forward sync complete");
    Ok(())
}
