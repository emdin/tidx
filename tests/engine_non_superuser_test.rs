//! The sync engine must run as an ordinary role, not the bootstrap superuser.
//!
//! 2026-11-06: tidx connected as the container's bootstrap superuser, so a
//! /query validator gap became file reads and code execution in the Postgres
//! container. `db/app_role.sql` (run once by the superuser) creates `tidx_app`,
//! which owns the schema and nothing more; these tests run the engine's own
//! work as that role and check what it can no longer do.

mod common;

use common::testdb::TestDb;
use serial_test::serial;
use tidx::db::{Pool, connected_as_superuser, create_pool, run_migrations};
use tidx::service::begin_public_query;

const APP_ROLE_SQL: &str = include_str!("../db/app_role.sql");
const APP_PASSWORD: &str = "tidx_app_test_password";

/// SQLSTATE "insufficient_privilege".
const INSUFFICIENT_PRIVILEGE: &str = "42501";

/// The test database URL with its credentials replaced by tidx_app's.
fn app_role_url() -> String {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let (scheme, rest) = url.split_once("://").expect("scheme");
    let host_and_db = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    format!("{scheme}://tidx_app:{APP_PASSWORD}@{host_and_db}")
}

async fn setup_app_role(db: &TestDb) {
    let conn = db.pool.get().await.expect("superuser connection");
    conn.batch_execute(APP_ROLE_SQL)
        .await
        .expect("app_role.sql must run as the superuser");
    conn.batch_execute(&format!("ALTER ROLE tidx_app PASSWORD '{APP_PASSWORD}'"))
        .await
        .unwrap();
}

async fn app_pool(db: &TestDb) -> Pool {
    setup_app_role(db).await;
    create_pool(&app_role_url()).await.expect("tidx_app pool")
}

async fn sqlstate(pool: &Pool, sql: &str) -> String {
    let conn = pool.get().await.unwrap();
    let err = conn
        .batch_execute(sql)
        .await
        .expect_err(&format!("tidx_app must not be able to run: {sql}"));
    err.code().map(|c| c.code().to_string()).unwrap_or_default()
}

#[tokio::test]
#[serial(db)]
async fn engine_role_is_an_ordinary_role_that_can_migrate() {
    let db = TestDb::empty().await;
    let pool = app_pool(&db).await;

    let conn = pool.get().await.unwrap();
    let row = conn
        .query_one(
            "SELECT current_user::text, rolsuper, rolcreaterole, rolcreatedb, rolreplication, rolbypassrls
             FROM pg_roles WHERE rolname = current_user",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "tidx_app");
    for (i, attr) in [
        "superuser",
        "createrole",
        "createdb",
        "replication",
        "bypassrls",
    ]
    .iter()
    .enumerate()
    {
        assert!(!row.get::<_, bool>(i + 1), "tidx_app must not have {attr}");
    }
    drop(conn);

    // Every boot runs the migrations: as owner of the schema it can.
    run_migrations(&pool).await.expect("migrations as tidx_app");
}

#[tokio::test]
#[serial(db)]
async fn engine_role_owns_every_object_after_setup() {
    let db = TestDb::empty().await;
    setup_app_role(&db).await;

    let conn = db.pool.get().await.unwrap();
    let foreign: Vec<String> = conn
        .query(
            "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = 'public' AND c.relkind IN ('r','p','v','m','S','f')
               AND c.relowner <> 'tidx_app'::regrole
             UNION ALL
             SELECT p.proname::text FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
             WHERE n.nspname = 'public' AND p.proowner <> 'tidx_app'::regrole",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert!(
        foreign.is_empty(),
        "objects not owned by tidx_app: {foreign:?}"
    );

    // query_role.sql re-grants USAGE on public to tidx_query on every boot;
    // without the grant option Postgres logs "no privileges were granted".
    let can_pass_on: bool = conn
        .query_one(
            "SELECT has_schema_privilege('tidx_app', 'public', 'USAGE WITH GRANT OPTION')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(can_pass_on);
}

#[tokio::test]
#[serial(db)]
async fn engine_role_writes_and_serves_public_queries() {
    let db = TestDb::empty().await;
    db.truncate_all().await;
    let pool = app_pool(&db).await;

    let conn = pool.get().await.unwrap();
    conn.batch_execute(
        "INSERT INTO sync_state (chain_id, head_num, synced_num) VALUES (424242, 7, 7)
         ON CONFLICT (chain_id) DO UPDATE SET head_num = 7",
    )
    .await
    .expect("tidx_app writes engine tables");
    conn.batch_execute("DELETE FROM sync_state WHERE chain_id = 424242")
        .await
        .unwrap();
    drop(conn);

    // The public path switches to tidx_query, which tidx_app must be allowed to do.
    let mut conn = pool.get().await.unwrap();
    let tx = begin_public_query(&mut conn, 5_000)
        .await
        .expect("SET LOCAL ROLE tidx_query from tidx_app");
    let who: String = tx
        .query_one("SELECT current_user::text", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(who, "tidx_query");
    tx.query_one("SELECT count(*) FROM blocks", &[])
        .await
        .unwrap();
}

#[tokio::test]
#[serial(db)]
async fn engine_role_has_no_superuser_powers() {
    let db = TestDb::empty().await;
    let pool = app_pool(&db).await;

    for sql in [
        "SELECT pg_read_file('/etc/passwd')",
        "SELECT pg_ls_dir('.')",
        "COPY (SELECT 1) TO PROGRAM 'true'",
        "COPY (SELECT 1) TO '/tmp/tidx_app_test'",
        "SELECT lo_import('/etc/passwd')",
        "CREATE ROLE tidx_app_escalation",
        "SET ROLE tidx",
    ] {
        assert_eq!(sqlstate(&pool, sql).await, INSUFFICIENT_PRIVILEGE, "{sql}");
    }
    // ALTER SYSTEM refuses inside a transaction block first; run it alone.
    let conn = pool.get().await.unwrap();
    let err = conn
        .simple_query("ALTER SYSTEM SET session_preload_libraries = 'x'")
        .await
        .expect_err("ALTER SYSTEM as tidx_app");
    assert_eq!(err.code().unwrap().code(), INSUFFICIENT_PRIVILEGE);
}

#[tokio::test]
#[serial(db)]
async fn app_role_script_is_idempotent() {
    let db = TestDb::empty().await;
    setup_app_role(&db).await;
    setup_app_role(&db).await;
}

#[tokio::test]
#[serial(db)]
async fn superuser_connection_is_detected() {
    // run_migrations warns at boot when this is true, so a reverted pg_url shows up.
    let db = TestDb::empty().await;
    assert!(connected_as_superuser(&db.pool).await.unwrap());
    let pool = app_pool(&db).await;
    assert!(!connected_as_superuser(&pool).await.unwrap());
}
