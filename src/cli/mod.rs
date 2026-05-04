pub mod backfill_denorm;
pub mod backfill_kaspa;
pub mod backfill_receipt_data;
pub mod backfill_traces;
pub mod backfill_withdrawals;
pub mod enrich_l1_senders;
pub mod import_blockscout;
pub mod init;
pub mod query;
pub mod status;
pub mod up;
pub mod upgrade;
pub mod views;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tidx")]
#[command(about = "High-throughput EVM and reth-based L2 indexer")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new config.toml
    Init(init::Args),
    /// Start syncing blocks from the chain (continuous) and serve HTTP API
    Up(up::Args),
    /// Show sync status
    Status(status::Args),
    /// Run a SQL query (use --signature to decode event logs)
    Query(query::Args),
    /// Manage ClickHouse materialized views
    Views(views::Args),
    /// Backfill txs.gas_used and txs.fee_payer from receipts
    BackfillReceiptData(backfill_receipt_data::Args),
    /// Backfill L2 block withdrawal allocations from RPC
    BackfillWithdrawals(backfill_withdrawals::Args),
    /// Backfill denormalized columns (txs.selector, logs."from") for pre-migration rows
    BackfillDenorm(backfill_denorm::Args),
    /// Backfill internal_txs (callTracer) for an L2 block range. Resumable.
    BackfillTraces(backfill_traces::Args),
    /// Backfill kaspa_l2_submissions + kaspa_entries by walking a Kaspa wRPC
    /// node's virtual chain. Idempotent. Use against local kaspad-archive or
    /// archival.kaspa.ws to recover provenance pre-dating the tidx deploy.
    BackfillKaspa(backfill_kaspa::Args),
    /// Populate l1_senders / l1_sender_amounts_sompi columns for kaspa_*
    /// tables by querying api.kaspa.org for each tx's resolved
    /// previous-outpoint addresses. Idempotent (only touches rows where
    /// l1_senders IS NULL).
    EnrichL1Senders(enrich_l1_senders::Args),
    /// Import verified contracts from a Blockscout explorer into local explorer metadata
    ImportBlockscout(import_blockscout::Args),
    /// Update tidx to the latest version
    Upgrade,
}
