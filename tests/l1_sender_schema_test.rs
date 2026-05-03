//! Schema test for Phase 1 of the Kaspa enrichment plan: verify that
//! `run_migrations` adds the L1-sender enrichment columns + indexes.
//!
//! These columns / indexes are a no-op at runtime (NULL until the
//! enrich-kaspa-senders CLI populates them) but adding them is the
//! foundation that subsequent backfill work will build on.

use tokio_postgres::types::Type;

mod common;
use common::testdb::TestDb;

/// Helper: assert a column exists with the expected type and nullability.
async fn assert_column(
    db: &TestDb,
    table: &str,
    column: &str,
    expected_data_type: &str,
    expected_udt_name: &str,
) {
    let conn = db.pool.get().await.expect("get conn");
    let row = conn
        .query_opt(
            "SELECT data_type, udt_name, is_nullable
             FROM information_schema.columns
             WHERE table_schema = 'public'
               AND table_name = $1
               AND column_name = $2",
            &[&table, &column],
        )
        .await
        .expect("query columns");
    let row = row.unwrap_or_else(|| {
        panic!("expected column {table}.{column} to exist after run_migrations, but it does not")
    });
    let data_type: String = row.get("data_type");
    let udt_name: String = row.get("udt_name");
    let is_nullable: String = row.get("is_nullable");
    assert_eq!(
        data_type, expected_data_type,
        "{table}.{column} data_type mismatch (expected {expected_data_type}, got {data_type})"
    );
    assert_eq!(
        udt_name, expected_udt_name,
        "{table}.{column} udt_name mismatch (expected {expected_udt_name}, got {udt_name})"
    );
    assert_eq!(
        is_nullable, "YES",
        "{table}.{column} must be nullable (so backfill can be lazy + idempotent)"
    );
    let _ = Type::TEXT_ARRAY; // silence unused-import warning when rustc is grumpy
}

/// Helper: assert an index exists.
async fn assert_index(db: &TestDb, index_name: &str) {
    let conn = db.pool.get().await.expect("get conn");
    let row = conn
        .query_opt(
            "SELECT indexname FROM pg_indexes
             WHERE schemaname = 'public' AND indexname = $1",
            &[&index_name],
        )
        .await
        .expect("query indexes");
    assert!(
        row.is_some(),
        "expected index {index_name} to exist after run_migrations"
    );
}

#[tokio::test]
async fn kaspa_entries_has_l1_sender_columns() {
    let db = TestDb::empty().await;
    assert_column(&db, "kaspa_entries", "l1_senders", "ARRAY", "_text").await;
    assert_column(
        &db,
        "kaspa_entries",
        "l1_sender_amounts_sompi",
        "ARRAY",
        "_int8",
    )
    .await;
    assert_column(
        &db,
        "kaspa_entries",
        "l1_enriched_at",
        "timestamp with time zone",
        "timestamptz",
    )
    .await;
}

#[tokio::test]
async fn kaspa_l2_submissions_has_l1_sender_columns() {
    let db = TestDb::empty().await;
    assert_column(&db, "kaspa_l2_submissions", "l1_senders", "ARRAY", "_text").await;
    assert_column(
        &db,
        "kaspa_l2_submissions",
        "l1_sender_amounts_sompi",
        "ARRAY",
        "_int8",
    )
    .await;
    assert_column(
        &db,
        "kaspa_l2_submissions",
        "l1_enriched_at",
        "timestamp with time zone",
        "timestamptz",
    )
    .await;
}

#[tokio::test]
async fn l2_withdrawals_has_kaspa_txid_link() {
    let db = TestDb::empty().await;
    assert_column(&db, "l2_withdrawals", "kaspa_txid", "bytea", "bytea").await;
    assert_index(&db, "idx_l2_withdrawals_kaspa_txid").await;
}

#[tokio::test]
async fn l1_sender_indexes_exist() {
    let db = TestDb::empty().await;
    assert_index(&db, "idx_kaspa_entries_l1_senders_gin").await;
    assert_index(&db, "idx_kaspa_l2_submissions_l1_senders_gin").await;
    assert_index(&db, "idx_kaspa_entries_enrichment_pending").await;
    assert_index(&db, "idx_kaspa_l2_submissions_enrichment_pending").await;
}

/// Migration must be idempotent — running it a second time must not error.
/// `TestDb::empty` only runs migrations once via OnceCell; call run_migrations
/// directly to trigger the second pass.
#[tokio::test]
async fn run_migrations_is_idempotent() {
    let db = TestDb::empty().await;
    tidx::db::run_migrations(&db.pool)
        .await
        .expect("run_migrations must be idempotent — second run should succeed");
    // and a third for good measure
    tidx::db::run_migrations(&db.pool)
        .await
        .expect("third run should also succeed");
    // verify schema is still in place after re-runs
    assert_column(&db, "kaspa_entries", "l1_senders", "ARRAY", "_text").await;
    assert_column(&db, "l2_withdrawals", "kaspa_txid", "bytea", "bytea").await;
}
