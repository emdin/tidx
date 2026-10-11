//! Kaspa provenance promotion window: pending rows become final after the
//! default delay (60 s), younger rows stay pending, and a virtual-chain reorg
//! still evicts them while they are pending.
//!
//! Requires `DATABASE_URL` → `tidx-test-pg`.

mod common;

use chrono::{Duration, Utc};
use common::testdb::TestDb;
use serial_test::serial;
use tidx::config::{Config, KaspaConfig};
use tidx::db::Pool;
use tidx::kaspa::writer::{KaspaProvenanceWriter, PendingEntry, PendingL2Submission};

#[test]
fn default_promotion_delay_is_one_minute() {
    assert_eq!(KaspaConfig::default().promotion_delay_secs, 60);

    let config: Config = toml::from_str(
        r#"
        [[chains]]
        name = "igra"
        chain_id = 38833
        rpc_url = "http://localhost:8545"
        pg_url = "postgres://localhost/test"

        [chains.kaspa]
        enabled = true
        "#,
    )
    .unwrap();
    assert_eq!(config.chains[0].kaspa.as_ref().unwrap().promotion_delay_secs, 60);
}

fn l2(tag: u8, block: u8, age_secs: i64) -> PendingL2Submission {
    PendingL2Submission {
        l2_tx_hash: [tag; 32],
        kaspa_txid: [tag ^ 0xff; 32],
        accepted_chain_block_hash: [block; 32],
        accepted_at: Utc::now() - Duration::seconds(age_secs),
    }
}

fn entry(tag: u8, block: u8, age_secs: i64) -> PendingEntry {
    PendingEntry {
        kaspa_txid: [tag; 32],
        recipient: [tag; 20],
        amount_sompi: 1_000,
        accepted_chain_block_hash: [block; 32],
        accepted_at: Utc::now() - Duration::seconds(age_secs),
    }
}

async fn count(pool: &Pool, sql: &str) -> i64 {
    pool.get().await.unwrap().query_one(sql, &[]).await.unwrap().get(0)
}

#[tokio::test]
#[serial(db)]
async fn default_window_promotes_after_a_minute_and_reorg_still_evicts_pending() {
    let db = TestDb::empty().await;
    db.pool
        .get()
        .await
        .unwrap()
        .batch_execute(
            "TRUNCATE kaspa_pending_l2_submissions, kaspa_pending_entries,
                      kaspa_l2_submissions, kaspa_entries",
        )
        .await
        .unwrap();
    let writer =
        KaspaProvenanceWriter::new(db.pool.clone(), KaspaConfig::default().promotion_delay_secs)
            .unwrap();

    // Old enough (61 s) vs still inside the window (10 s); 0x03 sits in a
    // block that a reorg is about to remove.
    writer
        .insert_pending(
            &[l2(0x01, 0xa1, 61), l2(0x02, 0xa2, 10), l2(0x03, 0xa3, 10)],
            &[entry(0x11, 0xa1, 61), entry(0x12, 0xa2, 10)],
        )
        .await
        .unwrap();

    let promoted = writer.promote_due().await.unwrap();
    assert_eq!(promoted.l2_submissions.len(), 1);
    assert_eq!(promoted.l2_submissions[0].l2_tx_hash, [0x01; 32]);
    assert_eq!(promoted.entries.len(), 1);
    assert_eq!(promoted.entries[0].kaspa_txid, [0x11; 32]);
    assert_eq!(count(&db.pool, "SELECT count(*) FROM kaspa_pending_l2_submissions").await, 2);
    assert_eq!(count(&db.pool, "SELECT count(*) FROM kaspa_pending_entries").await, 1);

    let evicted = writer.delete_pending_for_removed_blocks(&[[0xa3; 32]]).await.unwrap();
    assert_eq!(evicted, 1, "reorged pending row is dropped");
    assert_eq!(count(&db.pool, "SELECT count(*) FROM kaspa_l2_submissions").await, 1);
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM kaspa_pending_l2_submissions WHERE l2_tx_hash = '\\x0303030303030303030303030303030303030303030303030303030303030303'").await,
        0
    );
}
