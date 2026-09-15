//! A `/query` caller's `timeout_ms` must not persist on the pooled connection.
//!
//! `execute_query_postgres` used a session-level `SET statement_timeout`, which
//! stayed on the connection after it went back to the pool. The pool is shared
//! with the sync engine, so its gap scans inherited whatever timeout the last
//! `/query` caller chose — the default 5 s in practice, or `?timeout_ms=100`
//! from anyone who cared to, since `/query` is unauthenticated. That is how a
//! 6.7 s gap scan was being cancelled on every gap-fill tick (2026-09-14).
//!
//! A pool of exactly ONE connection makes the leak deterministic: whatever the
//! request does to that connection, the next borrower gets.

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::IntoMakeServiceWithConnectInfo;
use axum::http::{Request, StatusCode};
use axum::Router;
use common::testdb::TestDb;
use serial_test::serial;
use tidx::api;
use tidx::broadcast::Broadcaster;
use tidx::db::create_pool_with_size;
use tower::Service;

#[tokio::test]
#[serial(db)]
async fn query_timeout_does_not_leak_into_the_pooled_connection() {
    // Holds the test-DB lock and guarantees migrations; we borrow its URL.
    let _db = TestDb::empty().await;
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = create_pool_with_size(&url, 1).await.expect("single-connection pool");

    let mut pools = HashMap::new();
    pools.insert(1u64, pool.clone());
    let mut svc: IntoMakeServiceWithConnectInfo<Router, SocketAddr> =
        api::router(pools, 1, Arc::new(Broadcaster::new()))
            .into_make_service_with_connect_info::<SocketAddr>();
    let mut app = svc.call(SocketAddr::from(([127, 0, 0, 1], 0))).await.unwrap();

    let resp = app
        .call(
            Request::builder()
                .uri("/query?sql=SELECT%201%20AS%20one&chainId=1&timeout_ms=100")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "the query itself must still work");

    // The same (only) connection, as the next borrower — e.g. the sync engine.
    let conn = pool.get().await.unwrap();
    let row = conn.query_one("SHOW statement_timeout", &[]).await.unwrap();
    let timeout: String = row.get(0);
    assert_eq!(
        timeout, "0",
        "a /query caller's timeout_ms leaked onto the pooled connection (got {timeout}); \
         sync-engine scans borrowing it would be cancelled at that ceiling"
    );
}
