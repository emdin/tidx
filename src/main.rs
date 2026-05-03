use anyhow::Result;
use clap::Parser;

mod cli;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    // Install the rustls crypto provider once at process start, so any
    // wss:// connection (e.g. backfill-kaspa --rpc wss://archival.kaspa.ws)
    // doesn't panic on first TLS handshake. No-op when only plaintext ws://
    // is used elsewhere in the binary.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive("tidx=info".parse().unwrap());

    // Use JSON format if RUST_LOG_FORMAT=json
    if std::env::var("RUST_LOG_FORMAT").as_deref() == Ok("json") {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Init(args) => cli::init::run(args),
        Commands::Up(args) => cli::up::run(args).await,
        Commands::Status(args) => cli::status::run(args).await,
        Commands::Query(args) => cli::query::run(args).await,
        Commands::Views(args) => cli::views::run(args).await,
        Commands::BackfillReceiptData(args) => cli::backfill_receipt_data::run(args).await,
        Commands::BackfillWithdrawals(args) => cli::backfill_withdrawals::run(args).await,
        Commands::BackfillDenorm(args) => cli::backfill_denorm::run(args).await,
        Commands::BackfillTraces(args) => cli::backfill_traces::run(args).await,
        Commands::BackfillKaspa(args) => cli::backfill_kaspa::run(args).await,
        Commands::ImportBlockscout(args) => cli::import_blockscout::run(args).await,
        Commands::Upgrade => cli::upgrade::run(),
    }
}
