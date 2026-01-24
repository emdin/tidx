pub mod decoder;
pub mod engine;
pub mod fetcher;
pub mod replicator;
pub mod writer;

pub use replicator::{
    backfill_from_postgres, detect_all_gaps_duckdb, detect_gaps_duckdb, fill_gaps_from_postgres,
    get_sync_status, DuckDbSyncStatus, Replicator, ReplicatorHandle,
};
