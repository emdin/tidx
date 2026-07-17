//! Reorg archiving — round-trip and atomicity tests.
//!
//! Direct-call harness: seeds all six canonical tables + calls the reorg
//! mutation directly, no mock RPC. Covers 100% of the new archive path;
//! find_fork_point / handle_reorg glue is unchanged code and out of scope.

mod common;

use common::testdb::TestDb;
use serial_test::serial;
use tidx::db::Pool;
use tidx::sync::writer::delete_blocks_from;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

async fn truncate_all_incl_archive(pool: &Pool) {
    let conn = pool.get().await.expect("get conn");
    conn.batch_execute(
        "TRUNCATE
             blocks, txs, logs, receipts, l2_withdrawals, internal_txs,
             orphaned_blocks, orphaned_txs, orphaned_logs, orphaned_receipts,
             orphaned_l2_withdrawals, orphaned_internal_txs,
             reorgs, sync_state
         RESTART IDENTITY CASCADE",
    )
    .await
    .expect("truncate");
}

/// Seed contiguous blocks [start..=end] with one tx + one log + one receipt +
/// one internal_tx + one l2_withdrawal per block. Every row references its
/// block via block_num + block_timestamp (matches production PKs).
async fn seed_canonical_chain(pool: &Pool, start: i64, end: i64) {
    seed_canonical_chain_at(pool, start, end, "2026-01-01T00:00:00Z").await
}

async fn seed_canonical_chain_at(pool: &Pool, start: i64, end: i64, base_ts: &str) {
    // Deterministic timestamp: base_ts + num seconds. Computed SQL-side
    // to avoid tokio-postgres type-mapping quirks.
    let ts = format!("(TIMESTAMPTZ '{base_ts}' + ($1::bigint * INTERVAL '1 second'))");
    let ts = &ts;
    let conn = pool.get().await.expect("get conn");
    for num in start..=end {
        let hash = vec![num as u8; 32];
        let parent = vec![(num - 1) as u8; 32];
        let tx_hash: Vec<u8> = (0..32).map(|i| ((num * 7 + i) & 0xff) as u8).collect();
        let addr20 = vec![num as u8; 20];
        let ts_ms: i64 = num * 1000;

        conn.execute(
            &format!(
                "INSERT INTO blocks (num, hash, parent_hash, timestamp, timestamp_ms, gas_limit, gas_used, miner)
                 VALUES ($1, $2, $3, {ts}, $4, 30000000, 21000, $5)"),
            &[&num, &hash, &parent, &ts_ms, &addr20],
        ).await.expect("insert block");

        conn.execute(
            &format!("INSERT INTO txs
                (block_num, block_timestamp, idx, hash, type, \"from\", \"to\", value, input,
                 gas_limit, max_fee_per_gas, max_priority_fee_per_gas, gas_used,
                 nonce_key, nonce, call_count)
             VALUES ($1, {ts}, 0, $2, 2, $3, $3, '0', '\\x'::bytea,
                     21000, '0', '0', 21000,
                     $3, 0, 1)"),
            &[&num, &tx_hash, &addr20],
        ).await.expect("insert tx");

        conn.execute(
            &format!("INSERT INTO logs
                (block_num, block_timestamp, log_idx, tx_idx, tx_hash, address, data)
             VALUES ($1, {ts}, 0, 0, $2, $3, '\\x'::bytea)"),
            &[&num, &tx_hash, &addr20],
        ).await.expect("insert log");

        conn.execute(
            &format!("INSERT INTO receipts
                (block_num, block_timestamp, tx_idx, tx_hash, \"from\", gas_used, cumulative_gas_used, status)
             VALUES ($1, {ts}, 0, $2, $3, 21000, 21000, 1)"),
            &[&num, &tx_hash, &addr20],
        ).await.expect("insert receipt");

        conn.execute(
            &format!("INSERT INTO internal_txs
                (block_num, block_timestamp, tx_idx, tx_hash, depth, path_idx, call_type,
                 \"from\", \"to\", value, input, output, gas_used)
             VALUES ($1, {ts}, 0, $2, 1, 0, 'CALL',
                     $3, $3, '0', '\\x'::bytea, '\\x'::bytea, 21000)"),
            &[&num, &tx_hash, &addr20],
        ).await.expect("insert internal_tx");

        conn.execute(
            &format!("INSERT INTO l2_withdrawals
                (block_num, block_timestamp, idx, withdrawal_index, index_le, validator_index,
                 address, amount_gwei, amount_sompi)
             VALUES ($1, {ts}, 0, $1::text, $2, '0', $3, 0, 0)"),
            &[&num, &tx_hash, &addr20],
        ).await.expect("insert l2_withdrawal");
    }
}

async fn canonical_count(pool: &Pool, table: &str) -> i64 {
    let conn = pool.get().await.expect("get conn");
    let sql = format!("SELECT count(*) FROM {}", table);
    conn.query_one(&sql, &[]).await.expect("count").get(0)
}

async fn archive_count(pool: &Pool, table: &str) -> i64 {
    let conn = pool.get().await.expect("get conn");
    let sql = format!("SELECT count(*) FROM {}", table);
    conn.query_one(&sql, &[]).await.expect("count").get(0)
}

// ---------------------------------------------------------------------------
// TEST 1 — round-trip: rows moved, not lost; one reorgs event with correct counts.
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial(db)]
async fn test1_round_trip_moves_rows_and_records_event() {
    let db = TestDb::empty().await;
    truncate_all_incl_archive(&db.pool).await;

    // Seed blocks 1..=10, fork_point = 5 (blocks 6..=10 should be displaced).
    seed_canonical_chain(&db.pool, 1, 10).await;
    assert_eq!(canonical_count(&db.pool, "blocks").await, 10);
    assert_eq!(canonical_count(&db.pool, "txs").await, 10);

    // Call the reorg mutation. New behavior: DELETE from block 6 onwards
    // AND archive the same rows AND record one reorgs row.
    let deleted = delete_blocks_from(&db.pool, 6).await.expect("reorg");
    assert_eq!(deleted, 5, "should have removed 5 canonical blocks (6..=10)");

    // Canonical: blocks 1..=5 remain.
    assert_eq!(canonical_count(&db.pool, "blocks").await, 5);
    assert_eq!(canonical_count(&db.pool, "txs").await, 5);
    assert_eq!(canonical_count(&db.pool, "logs").await, 5);
    assert_eq!(canonical_count(&db.pool, "receipts").await, 5);
    assert_eq!(canonical_count(&db.pool, "internal_txs").await, 5);
    assert_eq!(canonical_count(&db.pool, "l2_withdrawals").await, 5);

    // Archive: 5 rows in each orphaned_* table (blocks 6..=10). RED today — expect 0.
    assert_eq!(archive_count(&db.pool, "orphaned_blocks").await, 5);
    assert_eq!(archive_count(&db.pool, "orphaned_txs").await, 5);
    assert_eq!(archive_count(&db.pool, "orphaned_logs").await, 5);
    assert_eq!(archive_count(&db.pool, "orphaned_receipts").await, 5);
    assert_eq!(archive_count(&db.pool, "orphaned_internal_txs").await, 5);
    assert_eq!(archive_count(&db.pool, "orphaned_l2_withdrawals").await, 5);

    // reorgs event row: one row, fork_point=5, prev_tip=10, depth=5.
    let conn = db.pool.get().await.expect("get conn");
    let row = conn
        .query_one(
            "SELECT fork_point, prev_tip, depth,
                    blocks_removed, txs_removed, logs_removed,
                    receipts_removed, internal_txs_removed, withdrawals_removed,
                    count_check_ok
               FROM reorgs",
            &[],
        )
        .await
        .expect("expected exactly one reorgs row");
    let fork_point: i64 = row.get(0);
    let prev_tip: i64 = row.get(1);
    let depth: i32 = row.get(2);
    let blocks_removed: i32 = row.get(3);
    let txs_removed: i32 = row.get(4);
    let logs_removed: i32 = row.get(5);
    let receipts_removed: i32 = row.get(6);
    let itx_removed: i32 = row.get(7);
    let wd_removed: i32 = row.get(8);
    let count_ok: bool = row.get(9);
    assert_eq!(fork_point, 5);
    assert_eq!(prev_tip, 10);
    assert_eq!(depth, 5);
    assert_eq!(blocks_removed, 5);
    assert_eq!(txs_removed, 5);
    assert_eq!(logs_removed, 5);
    assert_eq!(receipts_removed, 5);
    assert_eq!(itx_removed, 5);
    assert_eq!(wd_removed, 5);
    assert!(count_ok, "contiguous head, counts should match");

    // Archive rows carry the reorg_id from that event.
    let reorg_id: i64 = conn
        .query_one("SELECT id FROM reorgs", &[])
        .await
        .expect("get reorg id")
        .get(0);
    let orphaned_with_id: i64 = conn
        .query_one(
            "SELECT count(*) FROM orphaned_txs WHERE reorg_id = $1",
            &[&reorg_id],
        )
        .await
        .expect("count")
        .get(0);
    assert_eq!(orphaned_with_id, 5);
}

// ---------------------------------------------------------------------------
// TEST 2 — schema parity across all six pairs, in-DB assertion.
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial(db)]
async fn test2_schema_parity_all_pairs() {
    let db = TestDb::empty().await;
    let conn = db.pool.get().await.expect("get conn");
    // Runs the same DO $$ block as db/reorg_archive.sql's design check.
    // Raises EXCEPTION on drift, which surfaces as a tokio-postgres error.
    conn.batch_execute(
        r#"
        DO $$
        DECLARE
          pairs text[][] := ARRAY[
            ARRAY['txs','orphaned_txs'],
            ARRAY['logs','orphaned_logs'],
            ARRAY['receipts','orphaned_receipts'],
            ARRAY['internal_txs','orphaned_internal_txs'],
            ARRAY['l2_withdrawals','orphaned_l2_withdrawals'],
            ARRAY['blocks','orphaned_blocks']
          ];
          src text; arc text;
          src_arr text[]; arc_arr text[]; want text[];
        BEGIN
          FOR i IN 1..array_length(pairs, 1) LOOP
            src := pairs[i][1]; arc := pairs[i][2];
            SELECT array_agg(column_name || ':' || data_type ORDER BY ordinal_position)
              INTO src_arr FROM information_schema.columns WHERE table_name = src;
            SELECT array_agg(column_name || ':' || data_type ORDER BY ordinal_position)
              INTO arc_arr FROM information_schema.columns WHERE table_name = arc;
            want := ARRAY['reorg_id:bigint','orphaned_at:timestamp with time zone'] || src_arr;
            IF arc_arr <> want THEN
              RAISE EXCEPTION 'parity FAIL: % vs %', src, arc;
            END IF;
          END LOOP;
        END $$;
        "#,
    )
    .await
    .expect("schema parity assertion must pass");
}

// ---------------------------------------------------------------------------
// TEST 3 — resubmitted hash: same tx.hash canonical after being archived.
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial(db)]
async fn test3_resubmitted_hash_no_conflict() {
    let db = TestDb::empty().await;
    truncate_all_incl_archive(&db.pool).await;

    // Seed blocks 1..=5, reorg from block 3 (moves blocks 3-5 to archive).
    seed_canonical_chain(&db.pool, 1, 5).await;
    delete_blocks_from(&db.pool, 3).await.expect("reorg");

    // The tx from original block 3 is now in orphaned_txs with its specific hash.
    let orig_hash: Vec<u8> = (0..32).map(|i| ((3i64 * 7 + i) & 0xff) as u8).collect();
    let conn = db.pool.get().await.expect("get conn");
    let archived: i64 = conn
        .query_one(
            "SELECT count(*) FROM orphaned_txs WHERE hash = $1",
            &[&orig_hash],
        )
        .await
        .expect("count")
        .get(0);
    assert_eq!(archived, 1, "archived tx should be present");

    // Same hash re-lands in a fresh canonical block on the new chain.
    // Insert a canonical block at height 3 with a NEW timestamp (post-reorg),
    // then a tx there with the archived hash. Expected: no constraint violation.
    let new_ts: &str = "TIMESTAMPTZ '2027-01-01T00:00:00Z'";
    let addr20 = vec![0x33u8; 20];
    let hash_block = vec![0xAAu8; 32];
    let parent_block = vec![0x02u8; 32];
    let three: i64 = 3;
    let ts_ms: i64 = 33_000;
    conn.execute(
        &format!(
            "INSERT INTO blocks (num, hash, parent_hash, timestamp, timestamp_ms, gas_limit, gas_used, miner)
             VALUES ($1, $2, $3, {new_ts}, $4, 30000000, 21000, $5)"),
        &[&three, &hash_block, &parent_block, &ts_ms, &addr20],
    ).await.expect("insert new block 3");
    conn.execute(
        &format!("INSERT INTO txs
            (block_num, block_timestamp, idx, hash, type, \"from\", \"to\", value, input,
             gas_limit, max_fee_per_gas, max_priority_fee_per_gas, gas_used,
             nonce_key, nonce, call_count)
         VALUES ($1, {new_ts}, 0, $2, 2, $3, $3, '0', '\\x'::bytea,
                 21000, '0', '0', 21000,
                 $3, 0, 1)"),
        &[&three, &orig_hash, &addr20],
    ).await.expect("resubmit same hash — must not violate any constraint");

    let canon: i64 = conn
        .query_one("SELECT count(*) FROM txs WHERE hash = $1", &[&orig_hash])
        .await
        .expect("count")
        .get(0);
    assert_eq!(canon, 1, "resubmitted tx should sit in canonical table");

    // Both copies queryable — one canonical, one archived.
    let both: i64 = conn
        .query_one(
            "SELECT count(*) FROM (
                 SELECT 'c' FROM txs          WHERE hash = $1
                 UNION ALL
                 SELECT 'o' FROM orphaned_txs WHERE hash = $1
             ) t",
            &[&orig_hash],
        )
        .await
        .expect("count")
        .get(0);
    assert_eq!(both, 2);
}

// ---------------------------------------------------------------------------
// TEST 4 — atomicity: caller-owned rollback leaves canonical + archive clean.
// (Requires the transactional variant that accepts an external Transaction.
// Landed in P3; #[ignore] until then so P2 stays green on compile.)
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial(db)]
async fn test4_rollback_leaves_state_clean() {
    use tidx::sync::reorg_archive;

    let db = TestDb::empty().await;
    truncate_all_incl_archive(&db.pool).await;
    seed_canonical_chain(&db.pool, 1, 10).await;

    // Open a tx, run the mutation (succeeds — inside tx), then explicit rollback.
    let mut conn = db.pool.get().await.expect("get conn");
    let tx = conn.transaction().await.expect("begin tx");
    let result = reorg_archive::apply_reorg_mutation(&tx, 5)
        .await
        .expect("mutation must succeed before rollback exercise");
    assert_eq!(result.depth, 5);
    assert_eq!(result.blocks_removed, 5);

    // Simulate caller-side abort (equivalent to any error path after the
    // mutation completed but before commit).
    tx.rollback().await.expect("rollback");
    drop(conn);

    // Canonical rows untouched.
    assert_eq!(canonical_count(&db.pool, "blocks").await, 10);
    assert_eq!(canonical_count(&db.pool, "txs").await, 10);
    assert_eq!(canonical_count(&db.pool, "logs").await, 10);
    assert_eq!(canonical_count(&db.pool, "receipts").await, 10);
    assert_eq!(canonical_count(&db.pool, "internal_txs").await, 10);
    assert_eq!(canonical_count(&db.pool, "l2_withdrawals").await, 10);

    // Archive rows gone (the tx that inserted them rolled back).
    assert_eq!(archive_count(&db.pool, "orphaned_blocks").await, 0);
    assert_eq!(archive_count(&db.pool, "orphaned_txs").await, 0);
    assert_eq!(archive_count(&db.pool, "orphaned_logs").await, 0);
    assert_eq!(archive_count(&db.pool, "orphaned_receipts").await, 0);
    assert_eq!(archive_count(&db.pool, "orphaned_internal_txs").await, 0);
    assert_eq!(archive_count(&db.pool, "orphaned_l2_withdrawals").await, 0);

    // reorgs event row gone too — no orphan reorg_id references.
    let conn2 = db.pool.get().await.expect("get conn");
    let reorgs_count: i64 = conn2
        .query_one("SELECT count(*) FROM reorgs", &[])
        .await
        .expect("count")
        .get(0);
    assert_eq!(reorgs_count, 0, "reorgs row must roll back with the archive");
}

// ---------------------------------------------------------------------------
// TEST 6 — depth==0 guard: reorg call with fork_point >= tip must NOT
// write an empty reorgs row (spurious/no-op calls don't pollute the log).
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial(db)]
async fn test6_depth_zero_no_event_row() {
    use tidx::sync::reorg_archive;

    let db = TestDb::empty().await;
    truncate_all_incl_archive(&db.pool).await;
    seed_canonical_chain(&db.pool, 1, 10).await;

    // fork_point == prev_tip → nothing to displace → no event row.
    let mut conn = db.pool.get().await.expect("get conn");
    let tx = conn.transaction().await.expect("begin");
    let result = reorg_archive::apply_reorg_mutation(&tx, 10)
        .await
        .expect("depth=0 must succeed as a no-op");
    tx.commit().await.expect("commit");
    drop(conn);

    assert_eq!(result.depth, 0);
    assert_eq!(result.blocks_removed, 0);
    assert_eq!(result.reorg_id, 0, "no event row assigned");

    let conn2 = db.pool.get().await.expect("get conn");
    let reorgs_count: i64 = conn2
        .query_one("SELECT count(*) FROM reorgs", &[])
        .await
        .expect("count")
        .get(0);
    assert_eq!(reorgs_count, 0, "no reorgs row should be written");

    // Canonical rows untouched.
    assert_eq!(canonical_count(&db.pool, "blocks").await, 10);
    assert_eq!(canonical_count(&db.pool, "txs").await, 10);
}

// ---------------------------------------------------------------------------
// TEST 5 — repeat reorg: two events, distinct reorg_ids, correct tagging.
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial(db)]
async fn test5_repeat_reorg_distinct_ids() {
    let db = TestDb::empty().await;
    truncate_all_incl_archive(&db.pool).await;

    // Reorg A: seed 1..=10, cut at 8 (moves 8..=10). Then reseed 8..=12,
    // reorg B: cut at 6 (moves 6..=12 in the new state — that's blocks 6,7 from
    // the original chain and 8..=12 from the re-seeded chain).
    seed_canonical_chain(&db.pool, 1, 10).await;
    delete_blocks_from(&db.pool, 8).await.expect("reorg A");
    // Fresh chain B for blocks 8..=12 uses a later base timestamp so the
    // rewritten canonical rows don't collide with the archived ones on PK.
    seed_canonical_chain_at(&db.pool, 8, 12, "2027-01-01T00:00:00Z").await;
    delete_blocks_from(&db.pool, 6).await.expect("reorg B");

    let conn = db.pool.get().await.expect("get conn");

    // Two reorgs rows with distinct ids.
    let ids: Vec<i64> = conn
        .query("SELECT id FROM reorgs ORDER BY id", &[])
        .await
        .expect("select ids")
        .into_iter()
        .map(|r| r.get::<_, i64>(0))
        .collect();
    assert_eq!(ids.len(), 2, "two reorgs recorded");
    assert_ne!(ids[0], ids[1], "distinct ids");

    // The block-6/7 rows (from the FIRST chain, still in canonical after reorg A)
    // were displaced by reorg B and carry reorg B's id.
    let b_blocks: i64 = conn
        .query_one(
            "SELECT count(*) FROM orphaned_blocks WHERE reorg_id = $1 AND num IN (6, 7)",
            &[&ids[1]],
        )
        .await
        .expect("count")
        .get(0);
    assert_eq!(b_blocks, 2, "blocks 6 and 7 tagged with reorg B");

    // Reorg A's rows (blocks 8..=10 from first chain) stay tagged with A.
    let a_blocks: i64 = conn
        .query_one(
            "SELECT count(*) FROM orphaned_blocks WHERE reorg_id = $1",
            &[&ids[0]],
        )
        .await
        .expect("count")
        .get(0);
    assert_eq!(a_blocks, 3, "blocks 8..=10 tagged with reorg A");
}
