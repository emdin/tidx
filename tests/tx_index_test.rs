//! Integration tests for `kaspa_tx_index` upsert + schema. Requires
//! `DATABASE_URL` pointing at `tidx-test-pg` (default:
//! `postgresql://tidx:tidx@127.0.0.1:5434/tidx`).
//!
//! Tests cover the invariants the [`fee::FeeResolver`] relies on:
//! - Idempotent upserts (safe to re-run backfill).
//! - `ON CONFLICT DO NOTHING` on the `txid` PK — first-seen mapping wins,
//!   later duplicates are silently dropped.

use tidx::kaspa::tx_index::{self, TxIndexRow};
use tokio_postgres::types::Type;

mod common;
use common::testdb::TestDb;

fn h(tag: u8) -> [u8; 32] {
    let mut a = [0u8; 32];
    a[0] = tag;
    a
}

async fn truncate_tx_index(db: &TestDb) {
    let conn = db.pool.get().await.expect("get conn");
    conn.batch_execute("TRUNCATE kaspa_tx_index")
        .await
        .expect("truncate kaspa_tx_index");
}

async fn count_rows(db: &TestDb) -> i64 {
    let conn = db.pool.get().await.expect("get conn");
    conn.query_one("SELECT COUNT(*) FROM kaspa_tx_index", &[])
        .await
        .expect("count rows")
        .get(0)
}

#[tokio::test]
async fn schema_kaspa_tx_index_columns_have_expected_types() {
    let db = TestDb::empty().await;
    let conn = db.pool.get().await.expect("get conn");
    let row = conn
        .query_one(
            "SELECT data_type, is_nullable
             FROM information_schema.columns
             WHERE table_schema = 'public'
               AND table_name = 'kaspa_tx_index'
               AND column_name = $1",
            &[&"txid"],
        )
        .await
        .expect("query txid column");
    let txid_type: String = row.get("data_type");
    assert_eq!(txid_type, "bytea", "txid column must be bytea");
    let txid_nullable: String = row.get("is_nullable");
    assert_eq!(txid_nullable, "NO", "txid is the PK, must be NOT NULL");

    let block_hash_row = conn
        .query_one(
            "SELECT data_type, is_nullable
             FROM information_schema.columns
             WHERE table_schema = 'public'
               AND table_name = 'kaspa_tx_index'
               AND column_name = 'block_hash'",
            &[],
        )
        .await
        .expect("query block_hash column");
    assert_eq!(block_hash_row.get::<_, String>("data_type"), "bytea");
    assert_eq!(
        block_hash_row.get::<_, String>("is_nullable"),
        "NO",
        "block_hash is required — an index entry with no block is meaningless"
    );

    let _ = Type::BYTEA;
}

#[tokio::test]
async fn upsert_empty_batch_is_noop() {
    let db = TestDb::empty().await;
    truncate_tx_index(&db).await;
    let n = tx_index::upsert_batch(&db.pool, &[])
        .await
        .expect("empty upsert should succeed with 0 writes");
    assert_eq!(n, 0);
    assert_eq!(count_rows(&db).await, 0);
}

#[tokio::test]
async fn upsert_new_rows_writes_all_and_returns_count() {
    let db = TestDb::empty().await;
    truncate_tx_index(&db).await;

    let rows = vec![
        TxIndexRow {
            txid: h(0xAA),
            block_hash: h(0x10),
            daa_score: Some(1000),
        },
        TxIndexRow {
            txid: h(0xBB),
            block_hash: h(0x10),
            daa_score: Some(1000),
        },
        TxIndexRow {
            txid: h(0xCC),
            block_hash: h(0x11),
            daa_score: Some(1001),
        },
    ];
    let n = tx_index::upsert_batch(&db.pool, &rows)
        .await
        .expect("upsert 3 new rows");
    assert_eq!(n, 3, "all 3 rows should be new inserts");
    assert_eq!(count_rows(&db).await, 3);

    // Sanity check: mapping is retrievable and matches what we wrote.
    let conn = db.pool.get().await.expect("get conn");
    let row = conn
        .query_one(
            "SELECT block_hash, daa_score FROM kaspa_tx_index WHERE txid = $1",
            &[&h(0xAA).as_slice()],
        )
        .await
        .expect("select AA");
    let bh: Vec<u8> = row.get("block_hash");
    assert_eq!(bh.as_slice(), &h(0x10));
    let daa: Option<i64> = row.get("daa_score");
    assert_eq!(daa, Some(1000));
}

#[tokio::test]
async fn upsert_reinsert_same_batch_is_zero_writes() {
    let db = TestDb::empty().await;
    truncate_tx_index(&db).await;

    let rows = vec![TxIndexRow {
        txid: h(0xAA),
        block_hash: h(0x10),
        daa_score: Some(1000),
    }];
    tx_index::upsert_batch(&db.pool, &rows).await.expect("first");
    let n = tx_index::upsert_batch(&db.pool, &rows)
        .await
        .expect("second");
    assert_eq!(n, 0, "reinsert of existing (txid, block_hash) yields 0 writes");
    assert_eq!(count_rows(&db).await, 1, "still only one row");
}

#[tokio::test]
async fn upsert_conflicting_txid_first_wins() {
    // A Kaspa tx can appear in multiple concurrent blocks. Our resolver
    // only needs ONE valid (txid, block_hash) mapping — first seen wins.
    // Later inserts with the same txid but a different block_hash must
    // be silent no-ops, NOT errors.
    let db = TestDb::empty().await;
    truncate_tx_index(&db).await;

    let first = vec![TxIndexRow {
        txid: h(0xAA),
        block_hash: h(0x10),
        daa_score: Some(1000),
    }];
    let second_different_block = vec![TxIndexRow {
        txid: h(0xAA),
        block_hash: h(0x22), // different block
        daa_score: Some(1000),
    }];

    tx_index::upsert_batch(&db.pool, &first)
        .await
        .expect("insert first mapping");
    let n = tx_index::upsert_batch(&db.pool, &second_different_block)
        .await
        .expect("conflict must not error");
    assert_eq!(n, 0, "conflicting insert is a no-op, not a duplicate row");
    assert_eq!(count_rows(&db).await, 1);

    // Confirm first mapping was preserved (NOT overwritten to h(0x22)).
    let conn = db.pool.get().await.expect("get conn");
    let bh: Vec<u8> = conn
        .query_one(
            "SELECT block_hash FROM kaspa_tx_index WHERE txid = $1",
            &[&h(0xAA).as_slice()],
        )
        .await
        .expect("select AA")
        .get(0);
    assert_eq!(
        bh.as_slice(),
        &h(0x10),
        "first-seen block_hash must be preserved"
    );
}

#[tokio::test]
async fn upsert_batch_with_partial_conflicts_writes_only_new_rows() {
    // Realistic backfill scenario: half the batch is already indexed
    // (from a previous run or from realtime), half is new. The new half
    // must land; the old half must be silently skipped.
    let db = TestDb::empty().await;
    truncate_tx_index(&db).await;

    let existing = vec![
        TxIndexRow {
            txid: h(0xAA),
            block_hash: h(0x10),
            daa_score: Some(1000),
        },
        TxIndexRow {
            txid: h(0xBB),
            block_hash: h(0x10),
            daa_score: Some(1000),
        },
    ];
    tx_index::upsert_batch(&db.pool, &existing)
        .await
        .expect("seed existing");
    assert_eq!(count_rows(&db).await, 2);

    let mixed = vec![
        TxIndexRow {
            txid: h(0xAA), // exists — should skip
            block_hash: h(0x99),
            daa_score: Some(9999),
        },
        TxIndexRow {
            txid: h(0xCC), // new
            block_hash: h(0x11),
            daa_score: Some(1001),
        },
        TxIndexRow {
            txid: h(0xDD), // new
            block_hash: h(0x11),
            daa_score: Some(1001),
        },
    ];
    let n = tx_index::upsert_batch(&db.pool, &mixed)
        .await
        .expect("mixed upsert");
    assert_eq!(n, 2, "only CC + DD are new");
    assert_eq!(count_rows(&db).await, 4);
}
