use anyhow::Result;
use std::collections::HashSet;
use std::sync::Mutex;

use super::Pool;

const PARTITION_SIZE: u64 = 2_000_000;

pub struct PartitionManager {
    pool: Pool,
    created: Mutex<HashSet<u64>>,
}

impl PartitionManager {
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            created: Mutex::new(HashSet::new()),
        }
    }

    pub async fn ensure_partition(&self, block_num: u64) -> Result<()> {
        let partition_start = (block_num / PARTITION_SIZE) * PARTITION_SIZE;

        {
            let created = self.created.lock().unwrap();
            if created.contains(&partition_start) {
                return Ok(());
            }
        }

        let partition_end = partition_start + PARTITION_SIZE;
        let label = partition_start / 1_000_000;

        let sql = format!(
            r#"
            CREATE TABLE IF NOT EXISTS blocks_b{label}m
                PARTITION OF blocks FOR VALUES FROM ({partition_start}) TO ({partition_end});
            CREATE TABLE IF NOT EXISTS txs_b{label}m
                PARTITION OF txs FOR VALUES FROM ({partition_start}) TO ({partition_end});
            CREATE TABLE IF NOT EXISTS logs_b{label}m
                PARTITION OF logs FOR VALUES FROM ({partition_start}) TO ({partition_end});
            "#
        );

        self.pool.get().await?.batch_execute(&sql).await?;

        {
            let mut created = self.created.lock().unwrap();
            created.insert(partition_start);
        }

        Ok(())
    }
}
