//! Gap detection bounded to above the sync watermark.
//!
//! Background (2026-09-14 incident): every gap-fill tick ran `COUNT(*)` from
//! block 1 and, whenever any gap existed, an unbounded `LAG()` window over the
//! whole `blocks` table (6.7 s at 16.9M rows). That scan lost to a leaked 5 s
//! `statement_timeout` on every attempt, the tick failed, `synced_num` never
//! advanced, and the same doomed scan re-ran — 69 failures in 8 minutes while
//! `gap_blocks` grew at chain rate. `/status` ran the same scan per poll and
//! reported `gaps: []` when it timed out.
//!
//! `synced_num` is by definition the highest block below which the table is
//! verified contiguous, so scanning below it can never find anything. These
//! tests pin the bounded contract, including the one observable semantic
//! change: a gap BELOW the watermark is not reported (it cannot exist unless
//! the table was corrupted, and the startup audit is the full scan for that).

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
use tidx::sync::writer::{detect_gaps, gaps_above_watermark, has_gaps};
use tower::Service;

async fn insert_blocks(db: &TestDb, nums: &[i64]) {
    let conn = db.pool.get().await.expect("conn");
    for &num in nums {
        conn.execute(
            r#"INSERT INTO blocks (num, hash, parent_hash, timestamp, timestamp_ms, gas_limit, gas_used, miner)
               VALUES ($1, $2, $3, NOW(), $4, 1000000, 100000, $5)"#,
            &[
                &num,
                &vec![num as u8; 32],
                &vec![(num - 1) as u8; 32],
                &(num * 1000),
                &vec![0u8; 20],
            ],
        )
        .await
        .expect("insert block");
    }
}

/// 1..=10 minus `missing`.
fn one_to_ten_without(missing: &[i64]) -> Vec<i64> {
    (1..=10).filter(|n| !missing.contains(n)).collect()
}

#[tokio::test]
#[serial(db)]
async fn bounded_detect_gaps_only_sees_its_range() {
    let db = TestDb::empty().await;
    db.truncate_all().await;
    insert_blocks(&db, &one_to_ten_without(&[5, 8])).await;

    assert_eq!(detect_gaps(&db.pool, 1, 10).await.unwrap(), vec![(5, 5), (8, 8)]);
    // Below-watermark gap (5) is out of range; only 8 remains.
    assert_eq!(detect_gaps(&db.pool, 6, 10).await.unwrap(), vec![(8, 8)]);
    assert_eq!(detect_gaps(&db.pool, 9, 10).await.unwrap(), vec![]);
}

/// The window must START at the (present) watermark so a gap immediately
/// above it is detected — LAG has no previous row for the first one in range.
#[tokio::test]
#[serial(db)]
async fn bounded_detect_gaps_finds_gap_directly_above_watermark() {
    let db = TestDb::empty().await;
    db.truncate_all().await;
    insert_blocks(&db, &[3, 4, 7, 8]).await;

    assert_eq!(detect_gaps(&db.pool, 4, 8).await.unwrap(), vec![(5, 6)]);
}

#[tokio::test]
#[serial(db)]
async fn gaps_above_watermark_bounds_when_set_and_full_scans_when_zero() {
    let db = TestDb::empty().await;
    db.truncate_all().await;
    insert_blocks(&db, &one_to_ten_without(&[3, 8])).await;

    // Watermark 5: only the gap above it is a gap.
    assert_eq!(gaps_above_watermark(&db.pool, 5, 10).await.unwrap(), vec![(8, 8)]);
    // Fresh table (watermark 0): genesis-aware full scan, most recent first.
    assert_eq!(
        gaps_above_watermark(&db.pool, 0, 10).await.unwrap(),
        vec![(8, 8), (3, 3)]
    );
}

#[tokio::test]
#[serial(db)]
async fn has_gaps_bounded_to_watermark_ignores_gaps_below_it() {
    let db = TestDb::empty().await;
    db.truncate_all().await;
    insert_blocks(&db, &one_to_ten_without(&[3])).await;

    assert!(has_gaps(&db.pool, 1, 10).await.unwrap(), "full range sees the gap at 3");
    assert!(!has_gaps(&db.pool, 4, 10).await.unwrap(), "above the watermark is contiguous");
}

/// `/status` must report gaps above the watermark, and must not be doing a
/// full-table scan to do it. Pins the contract change: a gap below
/// `synced_num` is not reported (watermark 5 hides 3); watermark 0 reports all.
#[tokio::test]
#[serial(db)]
async fn status_reports_gaps_above_watermark_only() {
    let db = TestDb::empty().await;
    db.truncate_all().await;
    insert_blocks(&db, &one_to_ten_without(&[3, 8])).await;
    {
        let conn = db.pool.get().await.unwrap();
        conn.execute(
            "INSERT INTO sync_state (chain_id, head_num, synced_num, tip_num) VALUES (1, 10, 5, 10)
             ON CONFLICT (chain_id) DO UPDATE SET head_num = 10, synced_num = 5, tip_num = 10",
            &[],
        )
        .await
        .unwrap();
    }

    let mut pools = HashMap::new();
    pools.insert(1u64, db.pool.clone());
    let mut svc: IntoMakeServiceWithConnectInfo<Router, SocketAddr> =
        api::router(pools, 1, Arc::new(Broadcaster::new()))
            .into_make_service_with_connect_info::<SocketAddr>();
    let mut app = svc.call(SocketAddr::from(([127, 0, 0, 1], 0))).await.unwrap();

    let resp = app
        .call(Request::builder().uri("/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        json["chains"][0]["gaps"],
        serde_json::json!([[8, 8]]),
        "watermark 5 must report only the gap above it: {json}"
    );

    {
        let conn = db.pool.get().await.unwrap();
        conn.execute("UPDATE sync_state SET synced_num = 0 WHERE chain_id = 1", &[])
            .await
            .unwrap();
    }
    let resp = app
        .call(Request::builder().uri("/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        json["chains"][0]["gaps"],
        serde_json::json!([[8, 8], [3, 3]]),
        "watermark 0 must report every gap: {json}"
    );
}
