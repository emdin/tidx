//! Per-client rate limit on the public /query endpoint.
//!
//! The 2026-11-06 attacker sent up to ~400 /query requests a minute while
//! fuzzing the validator. The limit is per real client (X-Forwarded-For
//! behind the trusted proxy), not per TCP peer, or every public caller would
//! share Caddy's bucket.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::IntoMakeServiceWithConnectInfo;
use axum::http::{Request, StatusCode};
use tower::Service;

use tidx::api;
use tidx::broadcast::Broadcaster;
use tidx::config::HttpConfig;

const CADDY: &str = "172.18.0.4:40000";

fn limited_config(per_minute: u32, burst: u32, exempt: &[&str]) -> HttpConfig {
    HttpConfig {
        trusted_cidrs: vec!["172.16.0.0/12".to_string()],
        query_rate_limit_per_minute: per_minute,
        query_rate_limit_burst: burst,
        query_rate_limit_exempt: exempt.iter().map(|s| s.to_string()).collect(),
        ..HttpConfig::default()
    }
}

async fn service_for_peer(
    config: &HttpConfig,
    peer: &str,
) -> impl Service<Request<Body>, Response = axum::response::Response, Error = std::convert::Infallible>
{
    // No chain pools: a request that passes the limiter answers 400 (unknown
    // chain), which keeps this test free of any database.
    let router = api::router_with_options(
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        1,
        Arc::new(Broadcaster::new()),
        HashMap::new(),
        config,
    );
    let mut svc: IntoMakeServiceWithConnectInfo<Router, SocketAddr> =
        router.into_make_service_with_connect_info::<SocketAddr>();
    svc.call(peer.parse::<SocketAddr>().unwrap()).await.unwrap()
}

fn get_query(client: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().uri("/query?chainId=1&sql=SELECT%201");
    if let Some(ip) = client {
        b = b.header("x-forwarded-for", ip);
    }
    b.body(Body::empty()).unwrap()
}

fn post_query(client: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/query")
        .header("content-type", "application/json")
        .header("x-forwarded-for", client)
        .body(Body::from(r#"{"chainId":1,"sql":"SELECT 1"}"#))
        .unwrap()
}

async fn statuses<S>(svc: &mut S, n: usize, client: Option<&str>) -> Vec<StatusCode>
where
    S: Service<
            Request<Body>,
            Response = axum::response::Response,
            Error = std::convert::Infallible,
        >,
{
    let mut out = Vec::new();
    for _ in 0..n {
        out.push(svc.call(get_query(client)).await.unwrap().status());
    }
    out
}

#[tokio::test]
async fn client_over_its_burst_gets_429_with_retry_after() {
    let config = limited_config(60, 3, &[]);
    let mut svc = service_for_peer(&config, CADDY).await;

    let first = statuses(&mut svc, 3, Some("203.0.113.7")).await;
    assert!(
        first.iter().all(|s| *s != StatusCode::TOO_MANY_REQUESTS),
        "{first:?}"
    );

    let resp = svc.call(get_query(Some("203.0.113.7"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = resp.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=2).contains(&retry), "retry-after {retry}");
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ok"], false);
}

#[tokio::test]
async fn post_and_get_share_one_bucket() {
    let config = limited_config(60, 2, &[]);
    let mut svc = service_for_peer(&config, CADDY).await;

    assert_ne!(
        svc.call(post_query("203.0.113.7")).await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_ne!(
        svc.call(get_query(Some("203.0.113.7")))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        svc.call(post_query("203.0.113.7")).await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn clients_behind_the_proxy_have_separate_buckets() {
    let config = limited_config(60, 2, &[]);
    let mut svc = service_for_peer(&config, CADDY).await;

    statuses(&mut svc, 2, Some("203.0.113.7")).await;
    assert_eq!(
        svc.call(get_query(Some("203.0.113.7")))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    // Another public client must not be throttled by the first one.
    assert_ne!(
        svc.call(get_query(Some("198.51.100.9")))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn spoofed_xff_from_a_direct_client_does_not_buy_a_fresh_bucket() {
    // A client connecting directly (not via the trusted proxy) cannot rotate
    // X-Forwarded-For to dodge the limit: its own address is the key.
    let config = limited_config(60, 2, &[]);
    let mut svc = service_for_peer(&config, "203.0.113.7:50000").await;

    statuses(&mut svc, 2, Some("198.51.100.1")).await;
    assert_eq!(
        svc.call(get_query(Some("198.51.100.2")))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn exempt_client_is_never_limited() {
    // Sentinel's containers reach /query from the host's public address.
    let config = limited_config(60, 2, &["159.195.144.236/32"]);
    let mut svc = service_for_peer(&config, CADDY).await;

    let got = statuses(&mut svc, 10, Some("159.195.144.236")).await;
    assert!(
        got.iter().all(|s| *s != StatusCode::TOO_MANY_REQUESTS),
        "{got:?}"
    );
}

#[tokio::test]
async fn limit_is_off_by_default() {
    let config = HttpConfig {
        trusted_cidrs: vec!["172.16.0.0/12".to_string()],
        ..HttpConfig::default()
    };
    let mut svc = service_for_peer(&config, CADDY).await;

    let got = statuses(&mut svc, 50, Some("203.0.113.7")).await;
    assert!(got.iter().all(|s| *s != StatusCode::TOO_MANY_REQUESTS));
}

#[tokio::test]
async fn other_endpoints_are_not_limited() {
    let config = limited_config(60, 1, &[]);
    let mut svc = service_for_peer(&config, CADDY).await;

    for _ in 0..5 {
        let resp = svc
            .call(
                Request::builder()
                    .uri("/health")
                    .header("x-forwarded-for", "203.0.113.7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }
}
