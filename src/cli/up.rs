use anyhow::Result;
use clap::Args as ClapArgs;
use tracing::info;

use crate::config::Config;
use crate::db;
use crate::sync::engine::SyncEngine;

#[derive(ClapArgs)]
pub struct Args {
    /// RPC endpoint URL
    #[arg(long, env = "AK47_RPC_URL")]
    pub rpc: String,

    /// Database URL
    #[arg(long, env = "AK47_DATABASE_URL")]
    pub db: String,
}

pub async fn run(args: Args) -> Result<()> {
    let config = Config {
        rpc_url: args.rpc,
        database_url: args.db,
    };

    info!("Connecting to database...");
    let pool = db::create_pool(&config.database_url).await?;

    info!("Running migrations...");
    db::run_migrations(&pool).await?;

    info!("Starting sync engine...");
    let mut engine = SyncEngine::new(pool, &config.rpc_url).await?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);

    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Shutting down...");
        let _ = shutdown_tx.send(());
    });

    engine.run(shutdown_rx).await
}
