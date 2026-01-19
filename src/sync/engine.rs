use anyhow::Result;
use chrono::{TimeZone, Utc};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{debug, error, info};

use crate::config::chain_name;
use crate::db::Pool;
use crate::types::SyncState;

use super::decoder::{decode_block, decode_log, decode_transaction};
use super::fetcher::RpcClient;
use super::writer::{load_sync_state, save_sync_state, write_block, write_blocks, write_logs, write_txs};

pub struct SyncEngine {
    pool: Pool,
    rpc: RpcClient,
    chain_id: u64,
}

impl SyncEngine {
    pub async fn new(pool: Pool, rpc_url: &str) -> Result<Self> {
        let rpc = RpcClient::new(rpc_url);
        let chain_id = rpc.chain_id().await?;

        info!(
            chain_id = chain_id,
            network = chain_name(chain_id),
            "Connected to chain"
        );

        Ok(Self {
            pool,
            rpc,
            chain_id,
        })
    }

    pub async fn run(&mut self, mut shutdown: broadcast::Receiver<()>) -> Result<()> {
        loop {
            tokio::select! {
                _ = shutdown.recv() => {
                    info!("Shutting down sync engine");
                    break;
                }
                result = self.tick_pipelined() => {
                    if let Err(e) = result {
                        error!(error = %e, "Sync tick failed");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
        Ok(())
    }

    /// Pipelined sync: fetch next batch while writing current batch
    /// This overlaps network I/O with database I/O for better throughput
    async fn tick_pipelined(&mut self) -> Result<()> {
        let state = load_sync_state(&self.pool).await?.unwrap_or_default();
        let remote_head = self.rpc.latest_block_number().await?;

        let synced = if state.chain_id == self.chain_id {
            state.synced_num
        } else {
            0
        };

        if synced >= remote_head {
            tokio::time::sleep(Duration::from_millis(500)).await;
            return Ok(());
        }

        const BATCH_SIZE: u64 = 10;
        let mut current_from = synced + 1;

        // Fetch first batch
        let mut current_to = (current_from + BATCH_SIZE - 1).min(remote_head);
        let mut current_fetch = Some(self.fetch_range(current_from, current_to).await?);

        while current_from <= remote_head {
            let (blocks, _receipts, block_rows, all_txs, all_logs) = current_fetch.take().unwrap();

            // Start fetching next batch while we write current batch
            let next_from = current_to + 1;
            let next_to = (next_from + BATCH_SIZE - 1).min(remote_head);
            let has_next = next_from <= remote_head;

            let next_fetch_future = if has_next {
                Some(self.fetch_range(next_from, next_to))
            } else {
                None
            };

            // Write current batch (overlapped with next fetch)
            let write_future = async {
                write_blocks(&self.pool, &block_rows).await?;
                write_txs(&self.pool, &all_txs).await?;
                write_logs(&self.pool, &all_logs).await?;
                Ok::<_, anyhow::Error>(())
            };

            // Run fetch and write concurrently
            if let Some(fetch_fut) = next_fetch_future {
                let (write_result, fetch_result) = tokio::join!(write_future, fetch_fut);
                write_result?;
                current_fetch = Some(fetch_result?);
            } else {
                write_future.await?;
            }

            // Update sync state
            let new_state = SyncState {
                chain_id: self.chain_id,
                head_num: remote_head,
                synced_num: current_to,
            };
            save_sync_state(&self.pool, &new_state).await?;

            let tx_count = all_txs.len();
            let log_count = all_logs.len();
            debug!(
                from = current_from,
                to = current_to,
                blocks = blocks.len(),
                txs = tx_count,
                logs = log_count,
                "Wrote batch"
            );

            // Move to next batch
            current_from = next_from;
            current_to = next_to;
        }

        info!(
            synced = remote_head,
            lag = 0,
            "Caught up to head"
        );

        Ok(())
    }

    /// Fetch and decode a range of blocks (used by pipelined sync)
    async fn fetch_range(
        &self,
        from: u64,
        to: u64,
    ) -> Result<(
        Vec<crate::tempo::TempoBlock>,
        Vec<Vec<crate::tempo::TempoReceipt>>,
        Vec<crate::types::BlockRow>,
        Vec<crate::types::TxRow>,
        Vec<crate::types::LogRow>,
    )> {
        let (blocks, receipts) = tokio::try_join!(
            self.rpc.get_blocks_batch(from..=to),
            self.rpc.get_receipts_batch(from..=to)
        )?;

        let block_timestamps: HashMap<u64, _> = blocks
            .iter()
            .map(|b| {
                let ts = Utc.timestamp_opt(b.timestamp_u64() as i64, 0).unwrap();
                (b.number_u64(), ts)
            })
            .collect();

        let block_rows: Vec<_> = blocks.iter().map(decode_block).collect();

        let all_txs: Vec<_> = blocks
            .iter()
            .flat_map(|block| {
                block
                    .transactions()
                    .enumerate()
                    .map(|(i, tx)| decode_transaction(tx, block, i as u32))
            })
            .collect();

        let all_logs: Vec<_> = receipts
            .iter()
            .flatten()
            .flat_map(|receipt| {
                let block_num = receipt.block_number.to::<u64>();
                block_timestamps
                    .get(&block_num)
                    .map(|&ts| receipt.logs.iter().map(move |log| decode_log(log, ts)))
                    .into_iter()
                    .flatten()
            })
            .collect();

        Ok((blocks, receipts, block_rows, all_txs, all_logs))
    }

    #[allow(dead_code)]
    async fn tick(&mut self) -> Result<()> {
        let state = load_sync_state(&self.pool).await?.unwrap_or_default();
        let remote_head = self.rpc.latest_block_number().await?;

        let synced = if state.chain_id == self.chain_id {
            state.synced_num
        } else {
            0
        };

        if synced >= remote_head {
            tokio::time::sleep(Duration::from_millis(500)).await;
            return Ok(());
        }

        let from = synced + 1;
        // Smaller batches since receipts can be large (7k+ per block at high TPS)
        let to = (from + 9).min(remote_head);

        self.sync_range(from, to).await?;

        let new_state = SyncState {
            chain_id: self.chain_id,
            head_num: remote_head,
            synced_num: to,
        };
        save_sync_state(&self.pool, &new_state).await?;

        info!(
            from = from,
            to = to,
            head = remote_head,
            lag = remote_head - to,
            "Synced blocks"
        );

        Ok(())
    }

    async fn sync_range(&self, from: u64, to: u64) -> Result<()> {
        // Fetch blocks and receipts in parallel (receipts contain logs)
        let (blocks, receipts) = tokio::try_join!(
            self.rpc.get_blocks_batch(from..=to),
            self.rpc.get_receipts_batch(from..=to)
        )?;

        let block_timestamps: HashMap<u64, _> = blocks
            .iter()
            .map(|b| {
                let ts = Utc.timestamp_opt(b.timestamp_u64() as i64, 0).unwrap();
                (b.number_u64(), ts)
            })
            .collect();

        // Decode all blocks, transactions, and logs upfront
        let block_rows: Vec<_> = blocks.iter().map(decode_block).collect();

        let all_txs: Vec<_> = blocks
            .iter()
            .flat_map(|block| {
                block
                    .transactions()
                    .enumerate()
                    .map(|(i, tx)| decode_transaction(tx, block, i as u32))
            })
            .collect();

        let all_logs: Vec<_> = receipts
            .iter()
            .flatten()
            .flat_map(|receipt| {
                let block_num = receipt.block_number.to::<u64>();
                block_timestamps
                    .get(&block_num)
                    .map(|&ts| receipt.logs.iter().map(move |log| decode_log(log, ts)))
                    .into_iter()
                    .flatten()
            })
            .collect();

        // Batch write all data (single query per table)
        write_blocks(&self.pool, &block_rows).await?;
        write_txs(&self.pool, &all_txs).await?;
        write_logs(&self.pool, &all_logs).await?;

        Ok(())
    }

    pub async fn sync_block(&self, num: u64) -> Result<()> {
        let (block, receipts) = tokio::try_join!(
            self.rpc.get_block(num, true),
            self.rpc.get_block_receipts(num)
        )?;

        let block_row = decode_block(&block);
        let block_ts = Utc.timestamp_opt(block.timestamp_u64() as i64, 0).unwrap();
        write_block(&self.pool, &block_row).await?;

        let txs: Vec<_> = block
            .transactions()
            .enumerate()
            .map(|(i, tx)| decode_transaction(tx, &block, i as u32))
            .collect();

        write_txs(&self.pool, &txs).await?;

        // Extract logs from receipts
        let log_rows: Vec<_> = receipts
            .iter()
            .flat_map(|r| r.logs.iter().map(|log| decode_log(log, block_ts)))
            .collect();
        write_logs(&self.pool, &log_rows).await?;

        let new_state = SyncState {
            chain_id: self.chain_id,
            head_num: num,
            synced_num: num,
        };
        save_sync_state(&self.pool, &new_state).await?;

        Ok(())
    }
}
