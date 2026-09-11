//! End-to-end (/query HTTP path) tests for hex-literal rewriting.
//!
//! PR #26 shipped `topic_addr()` but its integration test bound the argument
//! as a parameter, bypassing the HTTP-layer `'0x…'`→bytea rewriter — so it
//! passed while `topic_addr('0x…')` was in fact broken through `/query`. These
//! tests drive the real router so a hex literal that is a *function argument*
//! (not a column comparison) is exercised exactly as a caller would hit it.

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
use tower::Service;

async fn service(
    pool: tidx::db::Pool,
) -> impl Service<Request<Body>, Response = axum::response::Response, Error = std::convert::Infallible>
{
    let mut pools = HashMap::new();
    pools.insert(1u64, pool);
    let mut svc: IntoMakeServiceWithConnectInfo<Router, SocketAddr> =
        api::router(pools, 1, Arc::new(Broadcaster::new()))
            .into_make_service_with_connect_info::<SocketAddr>();
    svc.call(SocketAddr::from(([127, 0, 0, 1], 0))).await.unwrap()
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// `topic_addr('0x…')` as a FUNCTION ARGUMENT must survive the rewriter intact
/// (the reported bug: it was mangled to `'\x…'` and errored "invalid
/// hexadecimal digit"). A bare SELECT keeps this Tempo-seed-free.
#[tokio::test]
#[serial(db)]
async fn topic_addr_with_0x_prefix_survives_the_rewriter_over_http() {
    let db = TestDb::empty().await;
    let mut app = service(db.pool.clone()).await;

    let resp = app
        .call(
            Request::builder()
                .method("GET")
                .uri("/query?sql=SELECT%20encode(topic_addr('0xC281cb25715EA8e46c9B916F22aE7c0F55b014d7'),'hex')%20AS%20t&chainId=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], true, "topic_addr('0x…') must not error via /query: {json}");
    assert_eq!(
        json["rows"][0][0],
        "000000000000000000000000c281cb25715ea8e46c9b916f22ae7c0f55b014d7",
        "topic_addr must still pad+lowercase correctly through the HTTP path"
    );
}

/// Same, via POST body — long SQL path must behave identically.
#[tokio::test]
#[serial(db)]
async fn topic_addr_with_0x_prefix_survives_over_post() {
    let db = TestDb::empty().await;
    let mut app = service(db.pool.clone()).await;

    let resp = app
        .call(
            Request::builder()
                .method("POST")
                .uri("/query")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"sql":"SELECT encode(topic_addr('0xC281cb25715EA8e46c9B916F22aE7c0F55b014d7'),'hex') AS t","chainId":1}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], true, "topic_addr('0x…') via POST must not error: {json}");
}

/// Regression: a hex literal that IS a column comparison operand must still be
/// converted to bytea — otherwise Postgres raises "operator does not exist:
/// bytea = text". `blocks` exists after migration (empty is fine, count = 0).
#[tokio::test]
#[serial(db)]
async fn column_comparison_literal_still_converts_to_bytea() {
    let db = TestDb::empty().await;
    let mut app = service(db.pool.clone()).await;

    let zeros = "0".repeat(64);
    let uri = format!("/query?sql=SELECT%20count(*)%20AS%20n%20FROM%20blocks%20WHERE%20hash%3D%270x{zeros}%27&chainId=1");
    let resp = app
        .call(Request::builder().method("GET").uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(
        json["ok"], true,
        "column = '0x…' must convert to bytea (else bytea=text error): {json}"
    );
}

/// Regression: IN-list of hex literals against a column still converts.
#[tokio::test]
#[serial(db)]
async fn in_list_literals_still_convert() {
    let db = TestDb::empty().await;
    let mut app = service(db.pool.clone()).await;

    let a = "0".repeat(64);
    let b = "1".repeat(64);
    let uri = format!(
        "/query?sql=SELECT%20count(*)%20AS%20n%20FROM%20blocks%20WHERE%20hash%20IN%20(%270x{a}%27%2C%270x{b}%27)&chainId=1"
    );
    let resp = app
        .call(Request::builder().method("GET").uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], true, "IN ('0x…','0x…') must convert both: {json}");
}
