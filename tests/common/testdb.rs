use ak47::db::{create_pool, run_migrations, Pool};
use ak47::sync::engine::SyncEngine;
use std::process::Stdio;
use tokio::sync::{Mutex, MutexGuard, OnceCell};

use super::tempo::TempoNode;

static MIGRATIONS_DONE: OnceCell<()> = OnceCell::const_new();
static TEST_LOCK: OnceCell<Mutex<()>> = OnceCell::const_new();
static SEEDING_DONE: OnceCell<()> = OnceCell::const_new();

pub struct TestDb {
    pub pool: Pool,
    pub url: String,
    _guard: MutexGuard<'static, ()>,
}

/// Options for seeding the test database
pub struct SeededOptions {
    /// Target transactions per second
    pub tps: u32,
    /// Duration in seconds to run the benchmark
    pub duration_secs: u32,
    /// Number of accounts to use
    pub accounts: u32,
    /// Minimum txs before skipping seeding
    pub min_txs: u32,
}

impl Default for SeededOptions {
    fn default() -> Self {
        Self {
            tps: 500,
            duration_secs: 30,
            accounts: 100,
            min_txs: 10_000,
        }
    }
}

impl SeededOptions {
    /// Light seeding: ~5k txs, fast for CI
    pub fn light() -> Self {
        Self {
            tps: 200,
            duration_secs: 10,
            accounts: 20,
            min_txs: 1_000,
        }
    }

    /// Heavy seeding: ~1M+ txs for benchmarks
    pub fn heavy() -> Self {
        Self {
            tps: 2000,
            duration_secs: 600,
            accounts: 1000,
            min_txs: 100_000,
        }
    }
}

fn get_test_db_url() -> String {
    std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://ak47:ak47@localhost:5433/ak47_test".to_string())
}

impl TestDb {
    /// Create a TestDb with auto-seeded data (default: light seeding)
    pub async fn new() -> Self {
        Self::with_options(SeededOptions::light()).await
    }

    /// Create a TestDb without seeding (for tests that need empty DB)
    pub async fn empty() -> Self {
        Self::init().await
    }

    /// Create a TestDb with custom seeding options
    pub async fn with_options(opts: SeededOptions) -> Self {
        let db = Self::init().await;
        db.ensure_seeded(opts).await;
        db
    }

    async fn init() -> Self {
        let lock = TEST_LOCK.get_or_init(|| async { Mutex::new(()) }).await;
        let guard = lock.lock().await;

        let url = get_test_db_url();
        let pool = create_pool(&url).await.expect("Failed to create pool");

        MIGRATIONS_DONE
            .get_or_init(|| async {
                run_migrations(&pool)
                    .await
                    .expect("Failed to run migrations");
            })
            .await;

        Self {
            pool,
            url,
            _guard: guard,
        }
    }

    async fn ensure_seeded(&self, opts: SeededOptions) {
        if self.tx_count().await >= opts.min_txs as i64 {
            return;
        }

        // Check if already seeded by another test
        if SEEDING_DONE.get().is_some() {
            return;
        }

        SEEDING_DONE
            .get_or_init(|| async {
                println!(
                    "Seeding database: {} TPS for {}s (~{} txs)",
                    opts.tps,
                    opts.duration_secs,
                    opts.tps * opts.duration_secs
                );
            })
            .await;

        // Run tempo-bench to generate transactions
        let tempo = TempoNode::from_env();
        tempo.wait_for_ready().await.expect("Tempo not ready");

        let status = tokio::process::Command::new("docker")
            .args([
                "run",
                "--rm",
                "--network",
                "host",
                "ghcr.io/tempoxyz/tempo-bench:latest",
                "run-max-tps",
                "--duration",
                &opts.duration_secs.to_string(),
                "--tps",
                &opts.tps.to_string(),
                "--accounts",
                &opts.accounts.to_string(),
                "--target-urls",
                &tempo.rpc_url,
                "--disable-2d-nonces",
                "--mnemonic",
                "test test test test test test test test test test test junk",
                "--tip20-weight",
                "3",
                "--erc20-weight",
                "2",
                "--swap-weight",
                "2",
            ])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await
            .expect("Failed to run tempo-bench");

        if !status.success() {
            // Bench may fail if already running or chain has conflicts - just log and continue
            tracing::warn!("tempo-bench exited with status: {} - chain may already have data", status);
        }

        // Sync the generated blocks
        println!("Syncing blocks to database...");
        let head = tempo.block_number().await.expect("Failed to get block number");

        let engine = SyncEngine::new(self.pool.clone(), &tempo.rpc_url)
            .await
            .expect("Failed to create sync engine");

        for block_num in 1..=head {
            engine
                .sync_block(block_num)
                .await
                .expect(&format!("Failed to sync block {}", block_num));

            if block_num % 100 == 0 {
                println!("Synced block {}/{}", block_num, head);
            }
        }

        println!(
            "Seeding complete: {} blocks, {} txs, {} logs",
            self.block_count().await,
            self.tx_count().await,
            self.log_count().await
        );
    }

    /// Check if database has been seeded with substantial data
    pub async fn is_seeded(&self) -> bool {
        self.tx_count().await > 100_000
    }

    pub async fn truncate_all(&self) {
        let conn = self.pool.get().await.expect("Failed to get connection");
        conn.batch_execute("TRUNCATE blocks, txs, logs, sync_state CASCADE")
            .await
            .expect("Failed to truncate tables");
    }

    pub async fn block_count(&self) -> i64 {
        let conn = self.pool.get().await.expect("Failed to get connection");
        let row = conn
            .query_one("SELECT COUNT(*) FROM blocks", &[])
            .await
            .expect("Failed to count blocks");
        row.get(0)
    }

    pub async fn tx_count(&self) -> i64 {
        let conn = self.pool.get().await.expect("Failed to get connection");
        let row = conn
            .query_one("SELECT COUNT(*) FROM txs", &[])
            .await
            .expect("Failed to count txs");
        row.get(0)
    }

    pub async fn log_count(&self) -> i64 {
        let conn = self.pool.get().await.expect("Failed to get connection");
        let row = conn
            .query_one("SELECT COUNT(*) FROM logs", &[])
            .await
            .expect("Failed to count logs");
        row.get(0)
    }
}
