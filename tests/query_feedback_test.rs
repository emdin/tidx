//! `/query` responses a consumer can act on (tidx user feedback, 2026-11-22).
//!
//! - Every response says how fresh each engine is. After the 2026-10-18 reboot
//!   the ClickHouse sink was off for 4.5 days and nothing in the responses
//!   showed it.
//! - A timeout says which engine, how long it ran and what to try instead: a
//!   `GROUP BY` over a busy address timed out on Postgres even at 30 s while
//!   ClickHouse answered in 0.1 s.
//! - A result the server cut at its row cap says so (`truncated`).
//! - `/tables` describes `l2_withdrawals` as what it is (entries into Igra) and
//!   doesn't promise automatic engine routing that doesn't exist.

mod common;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::IntoMakeServiceWithConnectInfo;
use axum::http::{Request, StatusCode};
use common::testdb::TestDb;
use serde_json::Value;
use serial_test::serial;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tidx::api;
use tidx::broadcast::Broadcaster;
use tidx::db::Pool;
use tower::Service;

/// Synthetic blocks live in their own height range so other tests' rows don't
/// change the counts.
const BASE: i64 = 900_000;

async fn seed_blocks(pool: &Pool, n: i64) {
    let c = pool.get().await.unwrap();
    c.execute(
        "DELETE FROM blocks WHERE num >= $1 AND num < $1 + 100000",
        &[&BASE],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO blocks (num, hash, parent_hash, timestamp, timestamp_ms, gas_limit, gas_used, miner)
         SELECT n, decode(lpad(to_hex(n), 64, '0'), 'hex'), decode(lpad(to_hex(n - 1), 64, '0'), 'hex'),
                now(), n * 1000, 1000000, 100000, decode(repeat('00', 20), 'hex')
         FROM generate_series($1::int8, $1::int8 + $2::int8 - 1) n",
        &[&BASE, &n],
    )
    .await
    .unwrap();
}

async fn set_sync_state(pool: &Pool, head: i64, tip: i64, synced: i64, age_secs: i64) {
    let c = pool.get().await.unwrap();
    c.execute("DELETE FROM sync_state WHERE chain_id = 1", &[])
        .await
        .unwrap();
    c.execute(
        "INSERT INTO sync_state (chain_id, head_num, synced_num, tip_num, updated_at)
         VALUES (1, $1, $2, $3, now() - make_interval(secs => $4::float8))",
        &[&head, &synced, &tip, &(age_secs as f64)],
    )
    .await
    .unwrap();
}

type App = axum::middleware::AddExtension<Router, axum::extract::ConnectInfo<SocketAddr>>;

async fn app(pool: &Pool) -> App {
    let mut pools = HashMap::new();
    pools.insert(1u64, pool.clone());
    let mut svc: IntoMakeServiceWithConnectInfo<Router, SocketAddr> =
        api::router(pools, 1, Arc::new(Broadcaster::new()))
            .into_make_service_with_connect_info::<SocketAddr>();
    svc.call(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap()
}

async fn get(app: &mut App, path_and_query: &str) -> (StatusCode, Value) {
    let resp = app
        .call(
            Request::builder()
                .uri(path_and_query)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 10 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).expect("JSON body"))
}

fn q(sql: &str, extra: &str) -> String {
    format!(
        "/query?chainId=1{extra}&sql={}",
        form_urlencoded::byte_serialize(sql.as_bytes()).collect::<String>()
    )
}

#[tokio::test]
#[serial(db)]
async fn every_response_reports_how_fresh_each_engine_is() {
    let db = TestDb::empty().await;
    set_sync_state(&db.pool, 1_000, 970, 960, 30).await;
    let mut app = app(&db.pool).await;

    let (status, body) = get(&mut app, &q("SELECT 1 AS one", "")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ix = &body["indexed"];
    assert_eq!(ix["chain_head"], 1_000, "{body}");
    assert_eq!(ix["postgres_head"], 970);
    assert_eq!(ix["postgres_contiguous"], 960);
    let age = ix["chain_head_age_secs"].as_i64().expect("age");
    assert!(
        (29..=40).contains(&age),
        "a stalled indexer shows as an old chain head: {age}"
    );
    assert!(
        ix["clickhouse_head"].is_null(),
        "no ClickHouse configured: {ix}"
    );

    let (status, body) = get(&mut app, &q("SELECT * FROM no_such_table", "")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["ok"], false);
    assert_eq!(
        body["engine"], "postgres",
        "errors say which engine: {body}"
    );
    assert_eq!(
        body["indexed"]["chain_head"], 1_000,
        "errors carry freshness too: {body}"
    );
}

#[tokio::test]
#[serial(db)]
async fn timeout_says_which_engine_how_long_and_what_to_try() {
    let db = TestDb::empty().await;
    seed_blocks(&db.pool, 3_000).await;
    set_sync_state(&db.pool, 10, 10, 10, 1).await;
    let mut app = app(&db.pool).await;

    let sql = format!(
        "SELECT count(*) FROM blocks a, blocks b, blocks c \
         WHERE a.num >= {BASE} AND b.num >= {BASE} AND c.num >= {BASE}"
    );
    let (status, body) = get(&mut app, &q(&sql, "&timeout_ms=200")).await;
    assert_eq!(status, StatusCode::REQUEST_TIMEOUT, "{body}");
    assert_eq!(body["ok"], false);
    assert_eq!(body["engine"], "postgres");
    assert_eq!(body["timeout_ms"], 200);
    let elapsed = body["elapsed_ms"].as_f64().expect("elapsed_ms");
    assert!(elapsed >= 150.0, "elapsed {elapsed}");
    let msg = body["error"].as_str().unwrap();
    assert!(msg.contains("postgres") && msg.contains("200"), "{msg}");
    assert!(
        msg.contains("engine=clickhouse"),
        "suggests the faster engine: {msg}"
    );
    assert!(body["indexed"].is_object());
}

#[tokio::test]
#[serial(db)]
async fn a_result_cut_at_the_server_cap_says_truncated() {
    let db = TestDb::empty().await;
    seed_blocks(&db.pool, 30).await;
    set_sync_state(&db.pool, 10, 10, 10, 1).await;
    let mut app = app(&db.pool).await;
    let sql = format!("SELECT num FROM blocks WHERE num >= {BASE} ORDER BY num");

    // The server's limit cut it: exactly `limit` rows come back, flagged.
    let (_, body) = get(&mut app, &q(&sql, "&limit=10")).await;
    assert_eq!(body["row_count"], 10, "{body}");
    assert_eq!(body["rows"].as_array().unwrap().len(), 10);
    assert_eq!(body["truncated"], true, "{body}");

    // Everything fit.
    let (_, body) = get(&mut app, &q(&sql, "&limit=100")).await;
    assert_eq!(body["row_count"], 30);
    assert_eq!(body["truncated"], false, "{body}");

    // Exactly `limit` rows exist: not truncated.
    let (_, body) = get(&mut app, &q(&sql, "&limit=30")).await;
    assert_eq!(body["row_count"], 30);
    assert_eq!(body["truncated"], false, "{body}");

    // The caller's own LIMIT is not a truncation.
    let (_, body) = get(&mut app, &q(&format!("{sql} LIMIT 5"), "")).await;
    assert_eq!(body["row_count"], 5);
    assert_eq!(body["truncated"], false, "{body}");
}

#[tokio::test]
#[serial(db)]
async fn tables_describes_l2_withdrawals_as_entries_and_routing_honestly() {
    let db = TestDb::empty().await;
    let mut app = app(&db.pool).await;
    let (status, body) = get(&mut app, "/tables").await;
    assert_eq!(status, StatusCode::OK);

    let t = body["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "l2_withdrawals")
        .expect("l2_withdrawals listed");
    let desc = t["description"].as_str().unwrap().to_lowercase();
    assert!(
        !desc.contains("exiting"),
        "they are entries into Igra, not exits: {desc}"
    );
    assert!(desc.contains("entr"), "{desc}");
    let cols: HashMap<&str, &str> = t["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap(),
                c["description"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(cols.contains_key("index_le"), "{cols:?}");
    assert!(cols.contains_key("kaspa_txid"), "{cols:?}");
    assert!(
        !cols["amount_sompi"].to_lowercase().contains("exits"),
        "{}",
        cols["amount_sompi"]
    );

    let text = body.to_string();
    assert!(
        !text.contains("Default routing picks"),
        "the code always uses Postgres unless engine=clickhouse"
    );
    assert!(
        text.contains("truncated"),
        "the truncated flag is documented"
    );
}

#[test]
fn server_imposed_limit_fetches_one_extra_row() {
    use tidx::query::enforce_limit_probe;
    assert_eq!(
        enforce_limit_probe("SELECT num FROM blocks", 10, 10_000),
        ("SELECT num FROM blocks LIMIT 11".to_string(), Some(10))
    );
    assert_eq!(
        enforce_limit_probe("SELECT num FROM blocks LIMIT 50000", 10, 10_000),
        (
            "SELECT num FROM blocks LIMIT 10001".to_string(),
            Some(10_000)
        )
    );
    assert_eq!(
        enforce_limit_probe("SELECT num FROM blocks LIMIT 5", 10, 10_000),
        ("SELECT num FROM blocks LIMIT 5".to_string(), None)
    );
}
