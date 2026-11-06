//! The public `/query` transaction must run without superuser rights.
//!
//! 2026-11-06: `/query` ran untrusted SQL as the bootstrap superuser `tidx`. A
//! validator gap let a caller call `pg_read_file` and `lo_export`, plant a
//! shared library via `session_preload_libraries`, and get code execution in
//! the Postgres container. The validator is fixed, but the database must not
//! depend on it alone: the public path now runs as `tidx_query`, a NOLOGIN
//! role with SELECT on the validator's allowlisted tables and nothing else.

mod common;

use common::testdb::TestDb;
use serial_test::serial;
use tidx::service::{QueryOptions, begin_public_query, execute_query_postgres};

/// SQLSTATE for "insufficient_privilege".
const INSUFFICIENT_PRIVILEGE: &str = "42501";
/// SQLSTATE for "read_only_sql_transaction".
const READ_ONLY_TRANSACTION: &str = "25006";
/// SQLSTATE for "active_sql_transaction" (ALTER SYSTEM refuses to run in one).
const IN_TRANSACTION_BLOCK: &str = "25001";

async fn sqlstate_of(db: &TestDb, sql: &str) -> String {
    let mut conn = db.pool.get().await.expect("pool");
    let tx = begin_public_query(&mut conn, 5_000).await.expect("begin");
    let err = tx
        .batch_execute(sql)
        .await
        .expect_err(&format!("must fail under the public role: {sql}"));
    err.code().map(|c| c.code().to_string()).unwrap_or_default()
}

#[tokio::test]
#[serial(db)]
async fn public_query_runs_as_unprivileged_role_and_can_still_read() {
    let db = TestDb::empty().await;
    let mut conn = db.pool.get().await.expect("pool");
    let tx = begin_public_query(&mut conn, 5_000).await.expect("begin");

    let who: String = tx
        .query_one("SELECT current_user::text", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(who, "tidx_query");

    tx.query_one("SELECT count(*) FROM blocks", &[])
        .await
        .expect("SELECT on blocks");
    tx.query_one("SELECT count(*) FROM kaspa_entries", &[])
        .await
        .expect("SELECT on kaspa_entries");
}

#[tokio::test]
#[serial(db)]
async fn public_query_cannot_touch_server_files_or_large_objects() {
    let db = TestDb::empty().await;
    for sql in [
        "SELECT pg_read_file('/etc/hostname')",
        "SELECT pg_read_binary_file('/etc/hostname')",
        "SELECT pg_ls_dir('.')",
        "SELECT pg_stat_file('postgresql.conf')",
        "SELECT lo_import('/etc/hostname')",
        "SELECT lo_export(lo_from_bytea(0, 'x'), '/tmp/tidx_query_test')",
        "SELECT pg_reload_conf()",
    ] {
        assert_eq!(sqlstate_of(&db, sql).await, INSUFFICIENT_PRIVILEGE, "{sql}");
    }
}

#[tokio::test]
#[serial(db)]
async fn public_query_transaction_is_read_only() {
    let db = TestDb::empty().await;
    for sql in [
        "CREATE TABLE tidx_query_probe (x int)",
        "UPDATE blocks SET num = num WHERE false",
        "ALTER SYSTEM SET session_preload_libraries = 'x'",
    ] {
        let code = sqlstate_of(&db, sql).await;
        assert!(
            [
                READ_ONLY_TRANSACTION,
                INSUFFICIENT_PRIVILEGE,
                IN_TRANSACTION_BLOCK
            ]
            .contains(&code.as_str()),
            "{sql}: got SQLSTATE {code}"
        );
    }
}

/// db/query_role.sql must grant every table the validator allows, or /query on
/// it fails with insufficient_privilege.
#[tokio::test]
#[serial(db)]
async fn public_role_can_read_every_allowlisted_table() {
    let db = TestDb::empty().await;
    let mut conn = db.pool.get().await.expect("pool");
    for table in tidx::query::ALLOWED_TABLES {
        let tx = begin_public_query(&mut conn, 5_000).await.expect("begin");
        tx.query_one(&format!("SELECT count(*) FROM {table}"), &[])
            .await
            .unwrap_or_else(|e| panic!("{table}: {e}"));
        tx.rollback().await.unwrap();
    }
}

/// Only the tables the validator allows; explorer/sync tables stay private even
/// if a validator bypass reaches them.
#[tokio::test]
#[serial(db)]
async fn public_role_cannot_read_tables_outside_the_allowlist() {
    let db = TestDb::empty().await;
    for table in ["address_labels", "contract_verifications", "token_metadata", "sync_state"] {
        let code = sqlstate_of(&db, &format!("SELECT count(*) FROM {table}")).await;
        assert_eq!(code, INSUFFICIENT_PRIVILEGE, "{table}");
    }
}

/// Backslashes in plain '...' must stay literal for Postgres, matching the
/// validator's parser.
#[tokio::test]
#[serial(db)]
async fn public_query_pins_standard_conforming_strings() {
    let db = TestDb::empty().await;
    let mut conn = db.pool.get().await.expect("pool");
    conn.batch_execute("SET standard_conforming_strings = off")
        .await
        .unwrap();
    let v: String = {
        let tx = begin_public_query(&mut conn, 5_000).await.expect("begin");
        tx.query_one("SHOW standard_conforming_strings", &[])
            .await
            .unwrap()
            .get(0)
    };
    conn.batch_execute("RESET standard_conforming_strings")
        .await
        .unwrap();
    assert_eq!(v, "on");
}

#[tokio::test]
#[serial(db)]
async fn public_query_rejects_escape_and_dollar_strings() {
    let db = TestDb::empty().await;
    for sql in [r"SELECT E'x' AS v", "SELECT $$x$$ AS v"] {
        let res = execute_query_postgres(&db.pool, sql, &[], &QueryOptions::default()).await;
        assert!(res.is_err(), "must reject: {sql}");
    }
}

/// The pool is shared with the sync engine, so the role switch must end with
/// the transaction (same leak class as the 2026-09-14 statement_timeout bug).
#[tokio::test]
#[serial(db)]
async fn role_switch_does_not_leak_into_the_pooled_connection() {
    let db = TestDb::empty().await;
    let mut conn = db.pool.get().await.expect("pool");
    let before: String = conn
        .query_one("SELECT current_user::text", &[])
        .await
        .unwrap()
        .get(0);
    {
        let tx = begin_public_query(&mut conn, 5_000).await.expect("begin");
        tx.query_one("SELECT 1", &[]).await.unwrap();
        tx.rollback().await.unwrap();
    }
    let after: String = conn
        .query_one("SELECT current_user::text", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(after, before);
    assert_ne!(after, "tidx_query");
}
