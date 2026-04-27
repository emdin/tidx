//! Integration test for `GET /tables`. The handler doesn't touch the database,
//! so this test runs without a real Postgres pool — empty `HashMap` works.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::IntoMakeServiceWithConnectInfo;
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::Service;

use tidx::api;
use tidx::broadcast::Broadcaster;

async fn make_test_service() -> impl Service<
    Request<Body>,
    Response = axum::response::Response,
    Error = std::convert::Infallible,
> {
    let pools: HashMap<u64, tidx::db::Pool> = HashMap::new();
    let chain_id = 1u64;
    let broadcaster = Arc::new(Broadcaster::new());

    let mut svc: IntoMakeServiceWithConnectInfo<Router, SocketAddr> =
        api::router(pools, chain_id, broadcaster)
            .into_make_service_with_connect_info::<SocketAddr>();
    svc.call(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap()
}

#[tokio::test]
async fn tables_endpoint_returns_200_with_full_schema() {
    let mut app = make_test_service().await;

    let response = app
        .call(
            Request::builder()
                .uri("/tables")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["ok"], true);

    let tables = json["tables"].as_array().expect("tables should be array");
    let names: Vec<&str> = tables
        .iter()
        .map(|t| t["name"].as_str().expect("table name should be a string"))
        .collect();

    // Spot-check the headline tables — full coverage is asserted by unit tests.
    for expected in &["blocks", "txs", "logs", "receipts", "l2_withdrawals"] {
        assert!(
            names.contains(expected),
            "table {expected:?} missing from /tables; got {names:?}"
        );
    }

    // Each table entry should have at least name, description, engines, columns.
    for t in tables {
        assert!(t["name"].as_str().is_some());
        assert!(t["description"].as_str().is_some());
        assert!(t["engines"].as_array().is_some());
        assert!(t["columns"].as_array().is_some());
    }

    let tips = json["tips"].as_array().expect("tips should be array");
    assert!(!tips.is_empty(), "tips should be populated");
}

#[tokio::test]
async fn tables_endpoint_response_can_be_consumed_as_json() {
    // Regression guard: the Content-Type header must say JSON so consumers
    // can parse without sniffing.
    let mut app = make_test_service().await;

    let response = app
        .call(
            Request::builder()
                .uri("/tables")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let ct = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .expect("response should have a Content-Type header");
    assert!(
        ct.starts_with("application/json"),
        "Content-Type should be application/json, got {ct:?}"
    );
}
