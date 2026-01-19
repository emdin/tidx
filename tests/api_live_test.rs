mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use ak47::api;
use ak47::broadcast::{BlockUpdate, Broadcaster};
use common::testdb::TestDb;

#[tokio::test]
async fn test_query_post_returns_json() {
    let db = TestDb::empty().await;
    let broadcaster = Arc::new(Broadcaster::new());
    let app = api::router(db.pool.clone(), broadcaster);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/query")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"sql": "SELECT 1 as value"}"#))
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
    assert!(json["columns"].as_array().is_some());
}

#[tokio::test]
async fn test_query_get_returns_sse() {
    let db = TestDb::empty().await;
    let broadcaster = Arc::new(Broadcaster::new());
    let app = api::router(db.pool.clone(), broadcaster.clone());

    // Spawn task to send a block update after a delay
    let broadcaster_clone = broadcaster.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        broadcaster_clone.send(BlockUpdate {
            chain_id: 4217,
            block_num: 1,
            block_hash: "0xabc".to_string(),
            tx_count: 0,
            log_count: 0,
            timestamp: 0,
        });
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/query?sql=SELECT%201%20as%20value")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Check content-type is SSE
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or(""));
    assert!(
        content_type.unwrap_or("").contains("text/event-stream"),
        "expected SSE content-type, got {:?}",
        content_type
    );
}

#[tokio::test]
async fn test_query_post_with_live_param_returns_sse() {
    let db = TestDb::empty().await;
    let broadcaster = Arc::new(Broadcaster::new());
    let app = api::router(db.pool.clone(), broadcaster);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/query?live=true")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"sql": "SELECT 1 as value"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or(""));
    assert!(
        content_type.unwrap_or("").contains("text/event-stream"),
        "expected SSE content-type, got {:?}",
        content_type
    );
}

#[tokio::test]
async fn test_health_endpoint() {
    let db = TestDb::empty().await;
    let broadcaster = Arc::new(Broadcaster::new());
    let app = api::router(db.pool.clone(), broadcaster);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"OK");
}

#[tokio::test]
async fn test_status_endpoint() {
    let db = TestDb::empty().await;
    let broadcaster = Arc::new(Broadcaster::new());
    let app = api::router(db.pool.clone(), broadcaster);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/status")
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

    // ok should be present (true or false depending on sync state)
    assert!(json.get("ok").is_some());
}
