//! Integration tests for `enrich_table` (L1 fee enrichment).
//!
//! Two shapes:
//! - Fast unit-flavored: run enrich_table with in-memory fakes to
//!   exercise the loop, batching, mixed outcomes, idempotency.
//! - Live smoke (#[ignore]): end-to-end against kaspad + PG.

use anyhow::Result;
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};

use kaspa_rpc_core::{
    RpcHash, RpcSubnetworkId, RpcTransaction, RpcTransactionVerboseData,
};

use tidx::kaspa::fee::{BlockFetcher, FeeResolver, TxLocator};
use tidx::kaspa::fee_enrichment;

mod common;
use common::testdb::TestDb;

fn h(tag: u8) -> [u8; 32] {
    let mut a = [0u8; 32];
    a[0] = tag;
    a
}

/// Locator seeded from a `HashMap<txid, block_hash>`. Absent → NotInIndex.
struct MapLocator {
    map: HashMap<[u8; 32], [u8; 32]>,
}

impl TxLocator for MapLocator {
    async fn locate(&self, txid: &[u8; 32]) -> Result<Option<[u8; 32]>> {
        Ok(self.map.get(txid).copied())
    }
}

/// Fetcher seeded from `HashMap<block_hash, Vec<RpcTransaction>>`.
struct MapFetcher {
    blocks: HashMap<[u8; 32], Vec<RpcTransaction>>,
    calls: AtomicU32,
}

impl BlockFetcher for MapFetcher {
    async fn get_block_txs(&self, hash: RpcHash) -> Result<Vec<RpcTransaction>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.blocks
            .get(&hash.as_bytes())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("fake fetcher: no such block {}", hash))
    }
}

fn tx(txid_tag: u8, inputs: Vec<([u8; 32], u32)>, outputs: Vec<u64>) -> RpcTransaction {
    let empty_spk: kaspa_rpc_core::RpcScriptPublicKey =
        serde_json::from_str(r#"{"version":0,"script":""}"#).unwrap();

    RpcTransaction {
        version: 0,
        inputs: inputs
            .into_iter()
            .map(|(prev_txid, index)| kaspa_rpc_core::RpcTransactionInput {
                previous_outpoint: kaspa_rpc_core::RpcTransactionOutpoint {
                    transaction_id: prev_txid.into(),
                    index,
                },
                signature_script: vec![],
                sequence: 0,
                sig_op_count: 0,
                verbose_data: None,
            })
            .collect(),
        outputs: outputs
            .into_iter()
            .map(|value| kaspa_rpc_core::RpcTransactionOutput {
                value,
                script_public_key: empty_spk.clone(),
                verbose_data: None,
            })
            .collect(),
        lock_time: 0,
        subnetwork_id: RpcSubnetworkId::from_byte(0),
        gas: 0,
        payload: vec![],
        mass: 0,
        verbose_data: Some(RpcTransactionVerboseData {
            transaction_id: h(txid_tag).into(),
            hash: h(txid_tag).into(),
            compute_mass: 0,
            block_hash: h(0xff).into(),
            block_time: 0,
        }),
    }
}

async fn truncate_targets(db: &TestDb) {
    let conn = db.pool.get().await.unwrap();
    conn.batch_execute(
        "TRUNCATE kaspa_l2_submissions;
         TRUNCATE kaspa_entries;
         TRUNCATE kaspa_tx_index;",
    )
    .await
    .unwrap();
}

async fn seed_submission(db: &TestDb, kaspa_txid: &[u8; 32], l2_hash: &[u8; 32]) {
    let conn = db.pool.get().await.unwrap();
    conn.execute(
        "INSERT INTO kaspa_l2_submissions (l2_tx_hash, kaspa_txid) VALUES ($1, $2)",
        &[&l2_hash.as_slice(), &kaspa_txid.as_slice()],
    )
    .await
    .unwrap();
}

async fn fetch_fee(db: &TestDb, table: &str, txid: &[u8; 32]) -> Option<i64> {
    let conn = db.pool.get().await.unwrap();
    let sql = format!("SELECT l1_fee_sompi FROM {table} WHERE kaspa_txid = $1");
    let row = conn.query_one(&sql, &[&txid.as_slice()]).await.unwrap();
    row.get(0)
}

#[tokio::test]
async fn enrich_writes_fee_for_resolvable_tx() {
    let db = TestDb::empty().await;
    truncate_targets(&db).await;

    // One target submission txid CC, seeded in the table.
    seed_submission(&db, &h(0xCC), &h(0x01)).await;

    // Fake resolver setup: target CC in block 0x12 with 1 input from
    // prev tx AA (in block 0x10) that has one output of 1000 sompi.
    // Target has one output of 900 sompi → fee = 100.
    let prev_a = tx(0xAA, vec![], vec![1000]);
    let target = tx(0xCC, vec![(h(0xAA), 0)], vec![900]);
    let locator = MapLocator {
        map: HashMap::from([(h(0xCC), h(0x12)), (h(0xAA), h(0x10))]),
    };
    let fetcher = MapFetcher {
        blocks: HashMap::from([(h(0x10), vec![prev_a]), (h(0x12), vec![target])]),
        calls: AtomicU32::new(0),
    };
    let resolver = FeeResolver::new(&locator, &fetcher);

    let stats = fee_enrichment::enrich_table(&db.pool, &resolver, "kaspa_l2_submissions", 100, None)
        .await
        .expect("enrich_table");

    assert_eq!(stats.rows_seen, 1);
    assert_eq!(stats.fees_written, 1);
    assert_eq!(stats.deferred_not_in_index, 0);
    assert_eq!(stats.failed, 0);
    assert_eq!(fetch_fee(&db, "kaspa_l2_submissions", &h(0xCC)).await, Some(100));
}

#[tokio::test]
async fn enrich_defers_row_when_txid_not_in_index() {
    let db = TestDb::empty().await;
    truncate_targets(&db).await;

    seed_submission(&db, &h(0xCC), &h(0x02)).await;

    // Empty locator → resolver returns NotInIndex → row stays NULL.
    let locator = MapLocator { map: HashMap::new() };
    let fetcher = MapFetcher {
        blocks: HashMap::new(),
        calls: AtomicU32::new(0),
    };
    let resolver = FeeResolver::new(&locator, &fetcher);

    let stats = fee_enrichment::enrich_table(&db.pool, &resolver, "kaspa_l2_submissions", 100, None)
        .await
        .expect("enrich_table");

    assert_eq!(stats.rows_seen, 1);
    assert_eq!(stats.fees_written, 0);
    assert_eq!(stats.deferred_not_in_index, 1);
    assert_eq!(fetch_fee(&db, "kaspa_l2_submissions", &h(0xCC)).await, None);
}

#[tokio::test]
async fn enrich_is_idempotent_across_reruns() {
    let db = TestDb::empty().await;
    truncate_targets(&db).await;

    seed_submission(&db, &h(0xCC), &h(0x03)).await;
    let prev = tx(0xAA, vec![], vec![500]);
    let target = tx(0xCC, vec![(h(0xAA), 0)], vec![400]);
    let locator = MapLocator {
        map: HashMap::from([(h(0xCC), h(0x12)), (h(0xAA), h(0x10))]),
    };
    let fetcher = MapFetcher {
        blocks: HashMap::from([(h(0x10), vec![prev]), (h(0x12), vec![target])]),
        calls: AtomicU32::new(0),
    };
    let resolver = FeeResolver::new(&locator, &fetcher);

    let first = fee_enrichment::enrich_table(&db.pool, &resolver, "kaspa_l2_submissions", 100, None)
        .await
        .unwrap();
    assert_eq!(first.fees_written, 1);
    let calls_after_first = fetcher.calls.load(Ordering::SeqCst);

    // Second pass: the WHERE l1_fee_sompi IS NULL filter should skip
    // the already-enriched row. No further RPC calls, no further writes.
    let second = fee_enrichment::enrich_table(&db.pool, &resolver, "kaspa_l2_submissions", 100, None)
        .await
        .unwrap();
    assert_eq!(second.rows_seen, 0, "no pending rows on second pass");
    assert_eq!(second.fees_written, 0);
    assert_eq!(
        fetcher.calls.load(Ordering::SeqCst),
        calls_after_first,
        "second pass should not touch RPC — the row is already enriched"
    );
    // Value should not have been touched.
    assert_eq!(fetch_fee(&db, "kaspa_l2_submissions", &h(0xCC)).await, Some(100));
}

#[tokio::test]
async fn enrich_processes_mixed_batch() {
    // 3 rows: one resolvable, one NotInIndex, one erroring (invariant
    // violation — indexed but its indexed block doesn't contain it).
    let db = TestDb::empty().await;
    truncate_targets(&db).await;

    seed_submission(&db, &h(0xC1), &h(0x11)).await; // resolvable
    seed_submission(&db, &h(0xC2), &h(0x12)).await; // not-in-index
    seed_submission(&db, &h(0xC3), &h(0x13)).await; // will error

    let prev = tx(0xAA, vec![], vec![1000]);
    let target = tx(0xC1, vec![(h(0xAA), 0)], vec![950]);
    let unrelated_block_contents = vec![prev.clone()]; // no C3 in it

    let locator = MapLocator {
        map: HashMap::from([
            (h(0xC1), h(0x10)),
            // C2 intentionally absent → NotInIndex
            (h(0xC3), h(0x20)), // points at block that doesn't contain C3
            (h(0xAA), h(0x0A)),
        ]),
    };
    let fetcher = MapFetcher {
        blocks: HashMap::from([
            (h(0x0A), vec![prev]),
            (h(0x10), vec![target]),
            (h(0x20), unrelated_block_contents),
        ]),
        calls: AtomicU32::new(0),
    };
    let resolver = FeeResolver::new(&locator, &fetcher);

    let stats = fee_enrichment::enrich_table(&db.pool, &resolver, "kaspa_l2_submissions", 100, None)
        .await
        .expect("enrich_table on mixed batch");

    assert_eq!(stats.rows_seen, 3);
    assert_eq!(stats.fees_written, 1, "C1 resolves cleanly");
    assert_eq!(stats.deferred_not_in_index, 1, "C2 lacks an index entry");
    assert_eq!(stats.failed, 1, "C3 is indexed but its block doesn't contain it");
    assert_eq!(fetch_fee(&db, "kaspa_l2_submissions", &h(0xC1)).await, Some(50));
    assert_eq!(fetch_fee(&db, "kaspa_l2_submissions", &h(0xC2)).await, None);
    assert_eq!(fetch_fee(&db, "kaspa_l2_submissions", &h(0xC3)).await, None);
}

#[tokio::test]
async fn enrich_respects_max_rows_cap() {
    // 5 seeded rows, cap at 2 — assert only 2 are processed and the
    // other 3 stay NULL for the next run.
    let db = TestDb::empty().await;
    truncate_targets(&db).await;

    // Set up 5 resolvable submissions, all pointing at a single prev tx
    // with abundant outputs so we don't hit output-index-out-of-bounds.
    let prev = tx(
        0xAA,
        vec![],
        vec![100, 200, 300, 400, 500, 600, 700, 800, 900, 1000],
    );
    let mut locator_map = HashMap::from([(h(0xAA), h(0x0A))]);
    let mut fetcher_blocks = HashMap::from([(h(0x0A), vec![prev])]);
    for i in 0..5 {
        let target_tag = 0xC0 + i;
        let target = tx(target_tag, vec![(h(0xAA), i as u32)], vec![10]);
        let block_tag = 0x50 + i;
        let l2_tag = 0x70 + i;
        seed_submission(&db, &h(target_tag), &h(l2_tag)).await;
        locator_map.insert(h(target_tag), h(block_tag));
        fetcher_blocks.insert(h(block_tag), vec![target]);
    }

    let locator = MapLocator { map: locator_map };
    let fetcher = MapFetcher {
        blocks: fetcher_blocks,
        calls: AtomicU32::new(0),
    };
    let resolver = FeeResolver::new(&locator, &fetcher);

    let stats = fee_enrichment::enrich_table(
        &db.pool,
        &resolver,
        "kaspa_l2_submissions",
        100,
        Some(2),
    )
    .await
    .unwrap();
    assert_eq!(stats.rows_seen, 2);
    assert_eq!(stats.fees_written, 2);

    // Verify DB state: exactly 2 rows have fee, 3 still NULL.
    let conn = db.pool.get().await.unwrap();
    let n_filled: i64 = conn
        .query_one(
            "SELECT COUNT(*) FROM kaspa_l2_submissions WHERE l1_fee_sompi IS NOT NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n_filled, 2);
    let n_null: i64 = conn
        .query_one(
            "SELECT COUNT(*) FROM kaspa_l2_submissions WHERE l1_fee_sompi IS NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n_null, 3, "remaining rows stay NULL for next run");
}

// -----------------------------------------------------------------------
// Silence unused-import warnings — some are pulled in only for trait
// impl paths and the rustc unused check doesn't see them.
// -----------------------------------------------------------------------
fn _keep_send_types_used<T: Future + Send>(_: T) {}
