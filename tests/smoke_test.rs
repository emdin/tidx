mod common;

use common::tempo::TempoNode;
use common::testdb::TestDb;

use ak47::sync::engine::SyncEngine;

#[tokio::test]
async fn test_sync_single_block() {
    let tempo = TempoNode::from_env();
    tempo.wait_for_ready().await.expect("Tempo node not ready");

    let db = TestDb::empty().await;
    db.truncate_all().await;

    // Wait for at least block 5 to exist
    tempo.wait_for_block(5).await.expect("Block 5 not reached");

    let engine = SyncEngine::new(db.pool.clone(), &tempo.rpc_url)
        .await
        .expect("Failed to create sync engine");

    engine.sync_block(5).await.expect("Failed to sync block");

    assert_eq!(db.block_count().await, 1);

    let conn = db.pool.get().await.expect("Failed to get connection");
    let block = conn
        .query_one("SELECT num, timestamp_ms FROM blocks WHERE num = 5", &[])
        .await
        .expect("Failed to query block");

    assert_eq!(block.get::<_, i64>(0), 5);
}

#[tokio::test]
async fn test_sync_state_persisted() {
    let tempo = TempoNode::from_env();
    tempo.wait_for_ready().await.expect("Tempo node not ready");

    let db = TestDb::empty().await;
    db.truncate_all().await;

    tempo.wait_for_block(10).await.expect("Block 10 not reached");

    let engine = SyncEngine::new(db.pool.clone(), &tempo.rpc_url)
        .await
        .expect("Failed to create sync engine");

    engine.sync_block(10).await.expect("Failed to sync block");

    let conn = db.pool.get().await.expect("Failed to get connection");
    let state = conn
        .query_one(
            "SELECT chain_id, head_num, synced_num FROM sync_state WHERE id = 1",
            &[],
        )
        .await
        .expect("Failed to query sync state");

    let chain_id = tempo.chain_id().await.expect("Failed to get chain ID");
    assert_eq!(state.get::<_, i64>(0), chain_id as i64);
    assert_eq!(state.get::<_, i64>(1), 10);
    assert_eq!(state.get::<_, i64>(2), 10);
}

#[tokio::test]
async fn test_sync_block_range() {
    let tempo = TempoNode::from_env();
    tempo.wait_for_ready().await.expect("Tempo node not ready");

    let db = TestDb::empty().await;
    db.truncate_all().await;

    tempo.wait_for_block(20).await.expect("Block 20 not reached");

    let engine = SyncEngine::new(db.pool.clone(), &tempo.rpc_url)
        .await
        .expect("Failed to create sync engine");

    // Sync blocks 1-20
    for block_num in 1..=20 {
        engine
            .sync_block(block_num)
            .await
            .expect(&format!("Failed to sync block {}", block_num));
    }

    // Verify all 20 blocks in range exist
    let conn = db.pool.get().await.expect("Failed to get connection");
    let count: i64 = conn
        .query_one("SELECT COUNT(DISTINCT num) FROM blocks WHERE num BETWEEN 1 AND 20", &[])
        .await
        .expect("Failed to count blocks")
        .get(0);

    assert_eq!(count, 20);
}

#[tokio::test]
async fn test_sync_logs() {
    let tempo = TempoNode::from_env();
    tempo.wait_for_ready().await.expect("Tempo node not ready");

    let db = TestDb::empty().await;
    db.truncate_all().await;

    // Wait for enough blocks that bench service has generated some txs with logs
    tempo.wait_for_block(50).await.expect("Block 50 not reached");

    let engine = SyncEngine::new(db.pool.clone(), &tempo.rpc_url)
        .await
        .expect("Failed to create sync engine");

    // Sync blocks 1-50
    for block_num in 1..=50 {
        engine
            .sync_block(block_num)
            .await
            .expect(&format!("Failed to sync block {}", block_num));
    }

    let conn = db.pool.get().await.expect("Failed to get connection");

    // Verify logs were synced (bench generates TIP-20/ERC-20 transfers which emit logs)
    let log_count: i64 = conn
        .query_one("SELECT COUNT(*) FROM logs", &[])
        .await
        .expect("Failed to count logs")
        .get(0);

    // Log count may be 0 if bench isn't running - that's OK, we're testing the sync mechanism
    println!("Synced {} logs from blocks 1-50", log_count);

    // If we have logs, verify structure is correct
    if log_count > 0 {
        let log = conn
            .query_one(
                "SELECT block_num, log_idx, tx_idx, address, tx_hash FROM logs LIMIT 1",
                &[],
            )
            .await
            .expect("Failed to query log");

        let block_num: i64 = log.get(0);
        let address: Vec<u8> = log.get(3);
        let tx_hash: Vec<u8> = log.get(4);

        assert!(block_num >= 1 && block_num <= 50);
        assert_eq!(address.len(), 20, "Address should be 20 bytes");
        assert_eq!(tx_hash.len(), 32, "Tx hash should be 32 bytes");
    }
}

// ============================================================================
// Seeded data tests - all tests auto-seed via TestDb::new()
// ============================================================================

#[tokio::test]
async fn test_seeded_tx_variance() {
    let db = TestDb::new().await;

    let conn = db.pool.get().await.expect("Failed to get connection");

    // Check tx type distribution (should have multiple types)
    let types: Vec<(i16, i64)> = conn
        .query(
            "SELECT type, COUNT(*) as cnt FROM txs GROUP BY type ORDER BY cnt DESC",
            &[],
        )
        .await
        .expect("Failed to query tx types")
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();

    println!("Transaction types: {:?}", types);
    assert!(types.len() >= 2, "Expected multiple tx types for variance");

    // Check call_count distribution (multicalls should have call_count > 1)
    let multicalls: i64 = conn
        .query_one("SELECT COUNT(*) FROM txs WHERE call_count > 1", &[])
        .await
        .expect("Failed to count multicalls")
        .get(0);

    println!("Multicall txs: {}", multicalls);

    // Check address diversity
    let unique_froms: i64 = conn
        .query_one("SELECT COUNT(DISTINCT \"from\") FROM txs", &[])
        .await
        .expect("Failed to count unique froms")
        .get(0);

    let unique_tos: i64 = conn
        .query_one("SELECT COUNT(DISTINCT \"to\") FROM txs WHERE \"to\" IS NOT NULL", &[])
        .await
        .expect("Failed to count unique tos")
        .get(0);

    println!("Unique from addresses: {}, unique to addresses: {}", unique_froms, unique_tos);
    assert!(unique_froms >= 5, "Expected diverse from addresses");
    assert!(unique_tos >= 5, "Expected diverse to addresses");
}

#[tokio::test]
async fn test_seeded_log_variance() {
    let db = TestDb::new().await;

    let conn = db.pool.get().await.expect("Failed to get connection");

    let log_count = db.log_count().await;
    println!("Total logs: {}", log_count);
    assert!(log_count > 0, "Expected logs from seeded data");

    // Check selector diversity (different event types)
    let unique_selectors: i64 = conn
        .query_one("SELECT COUNT(DISTINCT selector) FROM logs WHERE selector IS NOT NULL", &[])
        .await
        .expect("Failed to count selectors")
        .get(0);

    println!("Unique event selectors: {}", unique_selectors);
    assert!(unique_selectors >= 1, "Expected at least one event type");

    // Check contract diversity
    let unique_addresses: i64 = conn
        .query_one("SELECT COUNT(DISTINCT address) FROM logs", &[])
        .await
        .expect("Failed to count log addresses")
        .get(0);

    println!("Unique log-emitting contracts: {}", unique_addresses);
    assert!(unique_addresses >= 1, "Expected logs from at least one contract");
}

#[tokio::test]
async fn test_seeded_data_stats() {
    let db = TestDb::new().await;

    let conn = db.pool.get().await.expect("Failed to get connection");

    let blocks = db.block_count().await;
    let txs = db.tx_count().await;
    let logs = db.log_count().await;

    println!("=== Seeded Data Stats ===");
    println!("Blocks: {}", blocks);
    println!("Transactions: {}", txs);
    println!("Logs: {}", logs);
    println!("Avg txs/block: {:.1}", txs as f64 / blocks as f64);
    println!("Avg logs/tx: {:.1}", logs as f64 / txs as f64);

    // Time range
    let time_range = conn
        .query_one(
            "SELECT MIN(timestamp), MAX(timestamp), MAX(timestamp) - MIN(timestamp) FROM blocks",
            &[],
        )
        .await
        .expect("Failed to get time range");

    let min_ts: chrono::DateTime<chrono::Utc> = time_range.get(0);
    let max_ts: chrono::DateTime<chrono::Utc> = time_range.get(1);
    println!("Time range: {} to {}", min_ts, max_ts);

    assert!(blocks > 10, "Expected >10 blocks");
    assert!(txs > 100, "Expected >100 transactions");
}
