//! The ClickHouse copy of the Kaspa provenance tables.
//!
//! 2026-11-22, prod: ClickHouse `kaspa_l2_submissions` was keyed on
//! `l2_tx_hash` alone, so ReplacingMergeTree kept one Kaspa carrier per L2 tx
//! (Postgres 9.60 M rows for 9.50 M L2 txs, ClickHouse 9.21 M). Its hex was
//! also bare, while every other ClickHouse table and both engines' answers use
//! `0x…`, so `txs JOIN kaspa_l2_submissions ON hash = l2_tx_hash` matched
//! nothing on ClickHouse.
//!
//! Run with: CLICKHOUSE_URL=http://127.0.0.1:8124 cargo test --test kaspa_clickhouse_mirror_test

mod common;

use chrono::{TimeZone, Utc};
use common::clickhouse::TestClickHouse;
use serial_test::serial;
use tidx::kaspa::clickhouse::KaspaClickHouseMirror;
use tidx::kaspa::writer::{FinalEntry, FinalL2Submission};

const DB: &str = "tidx_kaspa_mirror_test";

async fn fresh_mirror() -> Option<(TestClickHouse, KaspaClickHouseMirror)> {
    let ch = TestClickHouse::new(DB).await.unwrap();
    if ch.wait_for_ready().await.is_err() {
        println!("ClickHouse not available, skipping test");
        return None;
    }
    ch.reset_database().await.unwrap();
    let mirror = KaspaClickHouseMirror::new(&ch.url, DB, None, None);
    mirror.ensure_schema().await.unwrap();
    Some((ch, mirror))
}

fn carrier(l2: u8, kaspa: u8) -> FinalL2Submission {
    FinalL2Submission {
        l2_tx_hash: [l2; 32],
        kaspa_txid: [kaspa; 32],
        created_at: Utc.timestamp_millis_opt(1_763_800_000_000).unwrap(),
    }
}

#[tokio::test]
#[serial(clickhouse)]
async fn every_kaspa_carrier_of_an_l2_tx_survives_merges() {
    let Some((ch, mirror)) = fresh_mirror().await else {
        return;
    };
    // Separate inserts make separate parts, as promotions minutes apart do.
    mirror
        .write_l2_submissions(&[carrier(0xaa, 0x01)])
        .await
        .unwrap();
    mirror
        .write_l2_submissions(&[carrier(0xaa, 0x02)])
        .await
        .unwrap();
    // A re-promotion of the same carrier is still one row.
    mirror
        .write_l2_submissions(&[carrier(0xaa, 0x02)])
        .await
        .unwrap();
    ch.query("OPTIMIZE TABLE kaspa_l2_submissions FINAL")
        .await
        .unwrap();

    let rows = ch
        .query("SELECT kaspa_txid FROM kaspa_l2_submissions FINAL ORDER BY kaspa_txid FORMAT TSV")
        .await
        .unwrap();
    assert_eq!(
        rows.lines().collect::<Vec<_>>(),
        [
            format!("0x{}", "01".repeat(32)),
            format!("0x{}", "02".repeat(32))
        ],
    );
}

#[tokio::test]
#[serial(clickhouse)]
async fn hashes_and_addresses_are_0x_hex_like_every_other_clickhouse_table() {
    let Some((ch, mirror)) = fresh_mirror().await else {
        return;
    };
    mirror
        .write_l2_submissions(&[carrier(0xab, 0xcd)])
        .await
        .unwrap();
    mirror
        .write_entries(&[FinalEntry {
            kaspa_txid: [0x97; 32],
            recipient: [0xee; 20],
            amount_sompi: 5,
            created_at: Utc.timestamp_millis_opt(1_763_800_000_000).unwrap(),
        }])
        .await
        .unwrap();

    let sub = ch
        .query("SELECT l2_tx_hash, kaspa_txid FROM kaspa_l2_submissions FORMAT TSV")
        .await
        .unwrap();
    assert_eq!(
        sub.trim_end(),
        format!("0x{}\t0x{}", "ab".repeat(32), "cd".repeat(32))
    );
    let entry = ch
        .query("SELECT kaspa_txid, recipient FROM kaspa_entries FORMAT TSV")
        .await
        .unwrap();
    assert_eq!(
        entry.trim_end(),
        format!("0x{}\t0x{}", "97".repeat(32), "ee".repeat(20))
    );
}

#[tokio::test]
#[serial(clickhouse)]
async fn tables_from_before_the_change_are_reported() {
    let Some((ch, mirror)) = fresh_mirror().await else {
        return;
    };
    mirror
        .write_l2_submissions(&[carrier(0xab, 0xcd)])
        .await
        .unwrap();
    assert_eq!(
        mirror.rebuild_reasons().await.unwrap(),
        Vec::<String>::new()
    );

    ch.query("DROP TABLE kaspa_l2_submissions").await.unwrap();
    ch.query(
        "CREATE TABLE kaspa_l2_submissions (l2_tx_hash String, kaspa_txid String, \
         created_at DateTime64(3, 'UTC')) ENGINE = ReplacingMergeTree() ORDER BY (l2_tx_hash)",
    )
    .await
    .unwrap();
    ch.query("INSERT INTO kaspa_entries SELECT '0x97b1', 'ee', 1, now64(3)")
        .await
        .unwrap();
    mirror.ensure_schema().await.unwrap();
    assert_eq!(
        mirror.rebuild_reasons().await.unwrap(),
        [
            "kaspa_l2_submissions is keyed on (l2_tx_hash)",
            "kaspa_entries has hex without 0x",
        ]
    );

    ch.query("INSERT INTO kaspa_l2_submissions SELECT 'ab', '0xcd', now64(3)")
        .await
        .unwrap();
    assert_eq!(
        mirror.rebuild_reasons().await.unwrap()[1],
        "kaspa_l2_submissions has hex without 0x"
    );
}
