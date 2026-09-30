//! Trace-outcome record: every traced tx gets an `ok` / `empty` / `failed`
//! row, failures are retried, repair passes select the right txs, and reorgs
//! drop outcomes for orphaned blocks.
//!
//! Requires `DATABASE_URL` → `tidx-test-pg`.

mod common;

use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
use chrono::{TimeZone, Utc};
use common::testdb::TestDb;
use serde_json::{Value, json};
use serial_test::serial;
use std::collections::HashMap;
use std::sync::Arc;
use tidx::db::Pool;
use tidx::sync::fetcher::RpcClient;
use tidx::sync::trace::{TraceOutcome, TraceStatus, trace_txs};
use tidx::sync::writer::{
    delete_blocks_from, load_txs_for_trace_repair, mark_existing_traces, write_trace_outcomes,
};
use tidx::types::TxRow;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// fake debug_traceTransaction server: per-hash scripted responses
// ---------------------------------------------------------------------------

/// What the fake RPC does on the n-th call for a hash: `Ok(frame)` or `Err`.
type Script = Vec<Result<Value, ()>>;

#[derive(Clone)]
struct Fake {
    scripts: Arc<Mutex<HashMap<String, Script>>>,
    calls: Arc<Mutex<HashMap<String, usize>>>,
}

async fn handler(State(f): State<Fake>, Json(body): Json<Value>) -> impl IntoResponse {
    assert_eq!(body["method"], "debug_traceTransaction");
    let hash = body["params"][0].as_str().unwrap().to_string();
    let n = {
        let mut c = f.calls.lock().await;
        let e = c.entry(hash.clone()).or_insert(0);
        *e += 1;
        *e - 1
    };
    let step = f.scripts.lock().await.get(&hash).and_then(|s| s.get(n).cloned());
    match step {
        Some(Ok(frame)) => Json(json!({"jsonrpc": "2.0", "id": 1, "result": frame})),
        _ => Json(json!({"jsonrpc": "2.0", "id": 1,
                         "error": {"code": -32000, "message": "trace unavailable"}})),
    }
}

async fn start_fake(scripts: HashMap<String, Script>) -> (RpcClient, Fake) {
    let fake = Fake {
        scripts: Arc::new(Mutex::new(scripts)),
        calls: Arc::new(Mutex::new(HashMap::new())),
    };
    let app = Router::new().route("/", post(handler)).with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (RpcClient::new(&format!("http://127.0.0.1:{}", addr.port())), fake)
}

fn frame(nested: usize) -> Value {
    let calls: Vec<Value> = (0..nested)
        .map(|_| json!({"type": "CALL", "from": "0x1111111111111111111111111111111111111111",
                        "to": "0x2222222222222222222222222222222222222222", "value": "0x5",
                        "gas": "0x100", "gasUsed": "0x80", "input": "0x", "output": "0x", "calls": []}))
        .collect();
    json!({"type": "CALL", "from": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
           "to": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "value": "0x0",
           "gas": "0x5208", "gasUsed": "0x5208", "input": "0x", "output": "0x", "calls": calls})
}

/// A frame whose nested call has a 2-byte address: RPC succeeds, flatten fails.
fn bad_frame() -> Value {
    let mut f = frame(1);
    f["calls"][0]["to"] = json!("0x1234");
    f
}

fn tx(block: i64, idx: i32, tag: u8) -> TxRow {
    TxRow {
        block_num: block,
        block_timestamp: Utc.timestamp_opt(1_700_000_000 + block, 0).unwrap(),
        idx,
        hash: vec![tag; 32],
        tx_type: 2,
        from: vec![0xaa; 20],
        to: Some(vec![0xbb; 20]),
        value: "0".into(),
        input: vec![],
        gas_limit: 21_000,
        max_fee_per_gas: "1".into(),
        max_priority_fee_per_gas: "1".into(),
        gas_used: None,
        nonce_key: vec![0],
        nonce: 0,
        fee_token: None,
        fee_payer: None,
        calls: None,
        call_count: 0,
        valid_before: None,
        valid_after: None,
        signature_type: None,
        selector: None,
    }
}

fn hex(tag: u8) -> String {
    format!("0x{}", hex::encode(vec![tag; 32]))
}

// ---------------------------------------------------------------------------
// trace_txs: outcome per tx, retry on failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn trace_txs_records_ok_empty_and_failed_with_retries() {
    let scripts = HashMap::from([
        (hex(0x01), vec![Ok(frame(2))]),                       // ok, 2 frames
        (hex(0x02), vec![Ok(frame(0))]),                       // empty
        (hex(0x03), vec![Err(()), Err(()), Ok(frame(1))]),     // ok on 3rd attempt
        (hex(0x04), vec![]),                                   // always fails
        (hex(0x05), vec![Ok(bad_frame()), Ok(bad_frame()), Ok(bad_frame())]), // RPC ok, unparseable
    ]);
    let (rpc, fake) = start_fake(scripts).await;
    let txs = vec![tx(10, 0, 0x01), tx(10, 1, 0x02), tx(10, 2, 0x03), tx(10, 3, 0x04), tx(10, 4, 0x05)];

    let batch = trace_txs(&rpc, &txs, 3).await;

    assert_eq!(batch.rows.len(), 3, "2 frames from tx1 + 1 from tx3");
    let by_hash: HashMap<Vec<u8>, &TraceOutcome> =
        batch.outcomes.iter().map(|o| (o.tx_hash.clone(), o)).collect();
    assert_eq!(by_hash.len(), 5, "one outcome per tx, always");
    let o5 = by_hash[&vec![0x05; 32]];
    assert_eq!((o5.status, o5.frames, o5.attempts), (TraceStatus::Failed, 0, 3));
    assert!(o5.error.as_deref().unwrap_or("").contains("expected 20"), "parse error is recorded: {:?}", o5.error);

    let o1 = by_hash[&vec![0x01; 32]];
    assert_eq!((o1.status, o1.frames, o1.attempts), (TraceStatus::Ok, 2, 1));
    let o2 = by_hash[&vec![0x02; 32]];
    assert_eq!((o2.status, o2.frames, o2.attempts), (TraceStatus::Empty, 0, 1));
    let o3 = by_hash[&vec![0x03; 32]];
    assert_eq!((o3.status, o3.frames, o3.attempts), (TraceStatus::Ok, 1, 3));
    let o4 = by_hash[&vec![0x04; 32]];
    assert_eq!((o4.status, o4.frames, o4.attempts), (TraceStatus::Failed, 0, 3));
    assert!(o4.error.as_deref().unwrap_or("").contains("trace unavailable"));

    let calls = fake.calls.lock().await;
    assert_eq!(calls[&hex(0x01)], 1);
    assert_eq!(calls[&hex(0x03)], 3);
    assert_eq!(calls[&hex(0x04)], 3, "bounded by max_attempts");
}

// ---------------------------------------------------------------------------
// write_trace_outcomes: upsert semantics
// ---------------------------------------------------------------------------

async fn outcome_row(pool: &Pool, tag: u8) -> (String, i32, i32, Option<String>) {
    let conn = pool.get().await.unwrap();
    let r = conn
        .query_one(
            "SELECT outcome, frames, attempts, error FROM trace_outcomes WHERE tx_hash = $1",
            &[&vec![tag; 32]],
        )
        .await
        .unwrap();
    (r.get(0), r.get(1), r.get(2), r.get(3))
}

fn outcome(tag: u8, block: i64, status: TraceStatus, frames: i32, attempts: i32, err: Option<&str>) -> TraceOutcome {
    TraceOutcome { tx_hash: vec![tag; 32], block_num: block, status, frames, attempts, error: err.map(str::to_string) }
}

#[tokio::test]
#[serial(db)]
async fn write_trace_outcomes_upserts_and_accumulates_attempts() {
    let db = TestDb::empty().await;
    let conn = db.pool.get().await.unwrap();
    conn.batch_execute("TRUNCATE trace_outcomes").await.unwrap();
    drop(conn);

    write_trace_outcomes(&db.pool, &[outcome(0x10, 5, TraceStatus::Failed, 0, 3, Some("boom"))]).await.unwrap();
    assert_eq!(outcome_row(&db.pool, 0x10).await, ("failed".into(), 0, 3, Some("boom".into())));

    // a later repair pass succeeds: status flips, attempts accumulate, error clears
    write_trace_outcomes(&db.pool, &[outcome(0x10, 5, TraceStatus::Ok, 4, 1, None)]).await.unwrap();
    assert_eq!(outcome_row(&db.pool, 0x10).await, ("ok".into(), 4, 4, None));

    // empty batch is a no-op, not an error
    write_trace_outcomes(&db.pool, &[]).await.unwrap();
}

// ---------------------------------------------------------------------------
// repair selection: failed + never-traced, never ok/empty
// ---------------------------------------------------------------------------

async fn seed_block_with_txs(pool: &Pool, block: i64, tags: &[u8]) {
    let conn = pool.get().await.unwrap();
    let ts = format!("(TIMESTAMPTZ '2026-01-01T00:00:00Z' + ({block}::bigint * INTERVAL '1 second'))");
    conn.execute(
        &format!("INSERT INTO blocks (num, hash, parent_hash, timestamp, timestamp_ms, gas_limit, gas_used, miner)
                  VALUES ($1, $2, $3, {ts}, $4, 30000000, 21000, $5) ON CONFLICT DO NOTHING"),
        &[&block, &vec![block as u8; 32], &vec![(block - 1) as u8; 32], &(block * 1000), &vec![0u8; 20]],
    ).await.unwrap();
    for (i, tag) in tags.iter().enumerate() {
        conn.execute(
            &format!("INSERT INTO txs (block_num, block_timestamp, idx, hash, type, \"from\", \"to\", value, input,
                                       gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce_key, nonce)
                      VALUES ($1, {ts}, $2, $3, 2, $4, $4, '0', '\\x', 21000, '1', '1', '\\x00', 0)
                      ON CONFLICT DO NOTHING"),
            &[&block, &(i as i32), &vec![*tag; 32], &vec![0xaa_u8; 20]],
        ).await.unwrap();
    }
}

#[tokio::test]
#[serial(db)]
async fn repair_selection_picks_failed_and_untraced_only() {
    let db = TestDb::empty().await;
    db.truncate_all().await;
    db.pool.get().await.unwrap().batch_execute("TRUNCATE trace_outcomes, internal_txs").await.unwrap();

    seed_block_with_txs(&db.pool, 100, &[0x21, 0x22, 0x23, 0x24]).await;
    write_trace_outcomes(&db.pool, &[
        outcome(0x21, 100, TraceStatus::Ok, 3, 1, None),
        outcome(0x22, 100, TraceStatus::Empty, 0, 1, None),
        outcome(0x23, 100, TraceStatus::Failed, 0, 3, Some("x")),
        // 0x24 never traced: no row
    ]).await.unwrap();

    let picked = load_txs_for_trace_repair(&db.pool, 100, 100, 10, 0, 1000).await.unwrap();
    let mut tags: Vec<u8> = picked.iter().map(|t| t.hash[0]).collect();
    tags.sort();
    assert_eq!(tags, vec![0x23, 0x24], "failed + never-traced, not ok/empty");

    // attempts cap: a failure that already burned its budget is left alone
    let picked = load_txs_for_trace_repair(&db.pool, 100, 100, 3, 0, 1000).await.unwrap();
    let tags: Vec<u8> = picked.iter().map(|t| t.hash[0]).collect();
    assert_eq!(tags, vec![0x24]);

    // age gate: the failure was recorded just now, so with retry_after=600s
    // only the never-traced tx qualifies — an RPC outage cannot burn the budget
    let picked = load_txs_for_trace_repair(&db.pool, 100, 100, 10, 600, 1000).await.unwrap();
    let tags: Vec<u8> = picked.iter().map(|t| t.hash[0]).collect();
    assert_eq!(tags, vec![0x24]);

    // limit honoured, oldest (block, idx) first
    let picked = load_txs_for_trace_repair(&db.pool, 100, 100, 10, 0, 1).await.unwrap();
    let tags: Vec<u8> = picked.iter().map(|t| t.hash[0]).collect();
    assert_eq!(tags, vec![0x23]);
}

// ---------------------------------------------------------------------------
// repair backoff: delay doubles per failed pass (3 attempts), capped at 24 h
// ---------------------------------------------------------------------------

async fn set_failed(pool: &Pool, tag: u8, block: i64, attempts: i32, ago_secs: i64) {
    write_trace_outcomes(pool, &[outcome(tag, block, TraceStatus::Failed, 0, attempts, Some("x"))]).await.unwrap();
    pool.get().await.unwrap()
        .execute(
            "UPDATE trace_outcomes SET attempts = $2, traced_at = now() - ($3::int8 * interval '1 second') WHERE tx_hash = $1",
            &[&vec![tag; 32], &attempts, &ago_secs],
        )
        .await
        .unwrap();
}

#[tokio::test]
#[serial(db)]
async fn repair_backoff_doubles_per_pass_and_caps_at_a_day() {
    let db = TestDb::empty().await;
    db.truncate_all().await;
    db.pool.get().await.unwrap().batch_execute("TRUNCATE trace_outcomes, internal_txs").await.unwrap();
    seed_block_with_txs(&db.pool, 400, &[0x41, 0x42, 0x43, 0x44, 0x45, 0x46]).await;

    // base 600 s. passes = attempts / 3; delay = 600 * 2^(passes-1), max 86 400.
    set_failed(&db.pool, 0x41, 400, 3, 11 * 60).await;          // 1 pass, due after 10 min  -> due
    set_failed(&db.pool, 0x42, 400, 6, 15 * 60).await;          // 2 passes, due after 20 min -> NOT due
    set_failed(&db.pool, 0x43, 400, 6, 21 * 60).await;          // 2 passes, 21 min           -> due
    set_failed(&db.pool, 0x44, 400, 12, 79 * 60).await;         // 4 passes, due after 80 min -> NOT due
    set_failed(&db.pool, 0x45, 400, 60, 23 * 3600).await;       // 20 passes, capped at 24 h  -> NOT due
    set_failed(&db.pool, 0x46, 400, 60, 25 * 3600).await;       // capped, 25 h               -> due (no attempt cap)

    let picked = load_txs_for_trace_repair(&db.pool, 400, 400, i32::MAX, 600, 1000).await.unwrap();
    let mut tags: Vec<u8> = picked.iter().map(|t| t.hash[0]).collect();
    tags.sort();
    assert_eq!(tags, vec![0x41, 0x43, 0x46]);

    // an explicit operator run (base 0) ignores the backoff entirely
    let picked = load_txs_for_trace_repair(&db.pool, 400, 400, i32::MAX, 0, 1000).await.unwrap();
    assert_eq!(picked.len(), 6);
}

// ---------------------------------------------------------------------------
// mark_existing_traces: frames present ⇒ ok, no RPC; nothing else touched
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial(db)]
async fn mark_existing_stamps_ok_only_on_traced_history() {
    let db = TestDb::empty().await;
    db.truncate_all().await;
    db.pool.get().await.unwrap().batch_execute("TRUNCATE trace_outcomes, internal_txs").await.unwrap();

    seed_block_with_txs(&db.pool, 300, &[0x31, 0x32, 0x33]).await;
    let conn = db.pool.get().await.unwrap();
    for (tag, tx_idx, path) in [(0x31u8, 0i32, 0i32), (0x31, 0, 1), (0x33, 2, 0)] {
        conn.execute(
            "INSERT INTO internal_txs (block_num, block_timestamp, tx_idx, tx_hash, depth, path_idx, call_type, \"from\", \"to\", value, input, output, gas_used, error)
             VALUES (300, TIMESTAMPTZ '2026-01-01T00:05:00Z', $4, $1, 1, $2, 'CALL', $3, $3, '1', '\\x', '\\x', 100, NULL)",
            &[&vec![tag; 32], &path, &vec![0xaa_u8; 20], &tx_idx],
        ).await.unwrap();
    }
    drop(conn);
    // 0x33 already has an outcome (failed, 5 attempts): must be left alone
    write_trace_outcomes(&db.pool, &[outcome(0x33, 300, TraceStatus::Failed, 0, 5, Some("x"))]).await.unwrap();

    let marked = mark_existing_traces(&db.pool, 300, 300).await.unwrap();
    assert_eq!(marked, 1, "only 0x31: has frames and no outcome");
    assert_eq!(outcome_row(&db.pool, 0x31).await, ("ok".into(), 2, 0, None));
    assert_eq!(outcome_row(&db.pool, 0x33).await, ("failed".into(), 0, 5, Some("x".into())));
    let n: i64 = db.pool.get().await.unwrap()
        .query_one("SELECT count(*) FROM trace_outcomes WHERE tx_hash = $1", &[&vec![0x32u8; 32]]).await.unwrap().get(0);
    assert_eq!(n, 0, "0x32 has no frames: stays untraced, never stamped ok");

    // idempotent
    assert_eq!(mark_existing_traces(&db.pool, 300, 300).await.unwrap(), 0);
}

// ---------------------------------------------------------------------------
// reorg: outcomes above the fork point are dropped with the orphaned blocks
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial(db)]
async fn reorg_drops_outcomes_for_orphaned_blocks() {
    let db = TestDb::empty().await;
    db.truncate_all().await;
    db.pool.get().await.unwrap()
        .batch_execute("TRUNCATE trace_outcomes, internal_txs, orphaned_blocks, orphaned_txs, orphaned_logs, orphaned_receipts, orphaned_l2_withdrawals, orphaned_internal_txs, reorgs RESTART IDENTITY CASCADE")
        .await.unwrap();

    for b in 200..=203 {
        seed_block_with_txs(&db.pool, b, &[b as u8]).await;
        write_trace_outcomes(&db.pool, &[outcome(b as u8, b, TraceStatus::Ok, 1, 1, None)]).await.unwrap();
    }

    delete_blocks_from(&db.pool, 202).await.unwrap();

    let conn = db.pool.get().await.unwrap();
    let left: Vec<i64> = conn
        .query("SELECT block_num FROM trace_outcomes ORDER BY block_num", &[])
        .await.unwrap().iter().map(|r| r.get(0)).collect();
    assert_eq!(left, vec![200, 201], "outcomes for orphaned blocks 202..203 are gone");
}
