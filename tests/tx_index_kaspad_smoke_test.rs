//! Smoke test: index a handful of blocks against a live kaspad, then
//! assert `extract_rows` + `upsert_batch` produce the invariants the
//! `FeeResolver` relies on:
//!
//!  - Every tx in every block visited has verbose_data (so extract_rows
//!    doesn't silently drop it).
//!  - Round-trip: after upsert, we can look up an arbitrary txid → block
//!    hash → getBlock(hash, true) → find that tx by id.
//!
//! Skipped by default (`#[ignore]`) because it needs a running kaspad
//! reachable at `KASPAD_WRPC_URL` (default `ws://127.0.0.1:17110`, i.e.
//! the loopback wRPC of `kaspad-mainnet` on this host). Run with:
//!
//!   DATABASE_URL=... cargo test --test tx_index_kaspad_smoke_test -- --ignored

use kaspa_rpc_core::api::rpc::RpcApi;
use tidx::kaspa::client::connect_borsh_wrpc;
use tidx::kaspa::tx_index::{self, TxIndexRow};

mod common;
use common::testdb::TestDb;

fn kaspad_url() -> String {
    std::env::var("KASPAD_WRPC_URL").unwrap_or_else(|_| "ws://127.0.0.1:17110".to_string())
}

#[tokio::test]
#[ignore]
async fn indexes_a_handful_of_recent_blocks_end_to_end() {
    // rustls provider — same setup as main.rs. If it fails to install
    // that's fine, another test may have already done it.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let db = TestDb::empty().await;
    // Fresh table for a clean row-count assertion.
    let conn = db.pool.get().await.expect("get conn");
    conn.batch_execute("TRUNCATE kaspa_tx_index")
        .await
        .expect("truncate");
    drop(conn);

    let client = connect_borsh_wrpc(&kaspad_url())
        .await
        .expect("connect to local kaspad");
    let dag = client
        .get_block_dag_info()
        .await
        .expect("get_block_dag_info");

    // Walk 5 blocks forward from pruning point — enough to guarantee at
    // least one non-coinbase tx and one mergeset visit without going
    // deep. `include_accepted_transaction_ids=false` — we don't need
    // acceptance info for indexing.
    let resp = client
        .get_virtual_chain_from_block(dag.pruning_point_hash, false, None)
        .await
        .expect("get_virtual_chain_from_block");
    let sample: Vec<_> = resp.added_chain_block_hashes.into_iter().take(5).collect();
    assert!(!sample.is_empty(), "expected at least one chain block");

    let mut all_rows: Vec<TxIndexRow> = Vec::new();
    let mut first_indexed: Option<([u8; 32], [u8; 32])> = None;
    for hash in &sample {
        let block = client
            .get_block(*hash, true)
            .await
            .unwrap_or_else(|e| panic!("get_block {hash} failed: {e}"));
        // Every tx must have verbose_data — otherwise extract_rows would
        // silently drop it and the fee resolver would be blind to the tx.
        for tx in &block.transactions {
            assert!(
                tx.verbose_data.is_some(),
                "block {hash} contained a tx with no verbose_data"
            );
        }
        let daa = i64::try_from(block.header.daa_score).ok();
        let rows = tx_index::extract_rows(hash.as_bytes(), daa, &block.transactions);
        if first_indexed.is_none() && !rows.is_empty() {
            first_indexed = Some((rows[0].txid, rows[0].block_hash));
        }
        all_rows.extend(rows);
    }
    assert!(
        !all_rows.is_empty(),
        "expected non-empty index rows from 5 blocks"
    );

    let n = tx_index::upsert_batch(&db.pool, &all_rows)
        .await
        .expect("upsert real batch");
    assert_eq!(
        n as usize,
        all_rows.len(),
        "fresh table + fresh txids: every row should insert"
    );

    // Round-trip: pick the first indexed txid, look it up, fetch the
    // block back from kaspad, confirm the tx is in it.
    let (probe_txid, expected_block) = first_indexed.expect("at least one row");
    let conn = db.pool.get().await.expect("get conn");
    let row = conn
        .query_one(
            "SELECT block_hash FROM kaspa_tx_index WHERE txid = $1",
            &[&probe_txid.as_slice()],
        )
        .await
        .expect("select probe");
    let block_hash_bytes: Vec<u8> = row.get(0);
    assert_eq!(block_hash_bytes.as_slice(), &expected_block);

    let block_hash_rpc: kaspa_rpc_core::RpcHash = expected_block.into();
    let block = client
        .get_block(block_hash_rpc, true)
        .await
        .expect("re-fetch the indexed block");
    let found = block.transactions.iter().any(|tx| {
        tx.verbose_data
            .as_ref()
            .is_some_and(|v| v.transaction_id.as_bytes() == probe_txid)
    });
    assert!(
        found,
        "indexed txid {} not present in its indexed block — invariant violation",
        hex::encode(probe_txid)
    );
}

/// Verifies the core invariant behind `--walk-back-days`: kaspad DOES serve
/// blocks below the pruning point via `GetBlock`, and their verbose_data
/// contains `selected_parent_hash` we can chain-walk with.
///
/// If this test breaks (kaspad's retention behavior changes), the backward
/// walk in `backfill-kaspa-tx-index` will silently narrow to just the
/// forward window again.
#[tokio::test]
#[ignore]
async fn kaspad_serves_blocks_below_pruning_point_and_exposes_selected_parent() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let client = connect_borsh_wrpc(&kaspad_url())
        .await
        .expect("connect to local kaspad");
    let dag = client
        .get_block_dag_info()
        .await
        .expect("get_block_dag_info");

    // Step 1: fetch pruning point block. It's always queryable.
    let pp = client
        .get_block(dag.pruning_point_hash, true)
        .await
        .expect("kaspad must serve its own pruning point");
    let verbose = pp
        .verbose_data
        .as_ref()
        .expect("pruning point block must have verbose_data");
    let pp_daa = pp.header.daa_score;

    // Step 2: walk backward via selected_parent 100 chain blocks.
    // Every step must succeed OR the test fails (documenting that kaspad
    // stopped serving history at a particular depth).
    let mut cursor = verbose.selected_parent_hash;
    let mut steps = 0usize;
    let mut deepest_daa = pp_daa;
    while steps < 100 {
        let block = client
            .get_block(cursor, true)
            .await
            .unwrap_or_else(|e| panic!(
                "kaspad stopped serving at step {steps} (hash {cursor}): {e}. \
                 Expected at least 100 chain blocks below the pruning point."
            ));
        let v = block
            .verbose_data
            .as_ref()
            .expect("verbose_data must be present on a getBlock response");
        deepest_daa = block.header.daa_score;
        if v.selected_parent_hash.as_bytes() == [0u8; 32] {
            // Reached genesis before 100 steps — unlikely on mainnet but ok.
            break;
        }
        cursor = v.selected_parent_hash;
        steps += 1;
    }

    // Step 3: we should be materially below the pruning point.
    let daa_delta = pp_daa.saturating_sub(deepest_daa);
    assert!(
        daa_delta > 0,
        "backward walk didn't move DAA at all — pp_daa={pp_daa}, deepest={deepest_daa}"
    );
    eprintln!(
        "backward walk: {steps} steps below PP, DAA delta {daa_delta} (~{:.2}h at 10 BPS)",
        daa_delta as f64 / 10.0 / 3600.0
    );
}
