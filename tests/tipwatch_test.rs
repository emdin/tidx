//! `tidx watch-tip` records every reorg a provider makes at the chain tip, at
//! any depth.
//!
//! The engine indexes `head_delay_blocks` (30) behind the tip, so its reorg
//! archive never sees the shallow reorgs Igra has all the time: Blockscout's
//! forked-block list for 2026-10-17..11-16 has 145,405 reorgs, deepest 9. These
//! tests drive the watcher against a scripted JSON-RPC chain served over real
//! HTTP and check what lands in the `tipwatch` schema. The watcher connects as
//! the least-privilege role from `db/tipwatch_role.sql`.

mod common;

use alloy::primitives::{B256, keccak256};
use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use common::testdb::TestDb;
use serde_json::{Value, json};
use serial_test::serial;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tidx::db::{Pool, create_pool};
use tidx::tipwatch::{StepOutcome, TipRpc, WatchOptions, Watcher, store};

const ROLE_PASSWORD: &str = "tidx_tipwatch_test_password";
/// SQLSTATE "insufficient_privilege".
const INSUFFICIENT_PRIVILEGE: &str = "42501";

// ---------------------------------------------------------------------------
// Scripted chain behind a JSON-RPC endpoint
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Blk {
    number: u64,
    hash: B256,
    parent: B256,
    ts: u64,
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    Pass,
    Http500,
    Malformed,
    RpcError,
    NullResult,
    WrongBlock,
}

#[derive(Default)]
struct Chain {
    canon: Vec<Blk>,
    by_hash: HashMap<B256, Blk>,
    branch: u64,
    lag: Option<u64>,
    faults: VecDeque<Fault>,
}

impl Chain {
    fn with_branch(branch: u64) -> Self {
        Self {
            branch,
            ..Default::default()
        }
    }

    fn mk(&mut self, number: u64, parent: B256) -> Blk {
        let hash = keccak256([number.to_be_bytes(), self.branch.to_be_bytes()].concat());
        let b = Blk {
            number,
            hash,
            parent,
            ts: 1_790_000_000 + number,
        };
        self.by_hash.insert(hash, b);
        b
    }

    fn extend(&mut self, n: u64) {
        for _ in 0..n {
            let (number, parent) = match self.canon.last() {
                Some(t) => (t.number + 1, t.hash),
                None => (0, B256::ZERO),
            };
            let b = self.mk(number, parent);
            self.canon.push(b);
        }
    }

    /// Replaces the top `depth` blocks with `new_len` blocks of a new branch.
    fn reorg(&mut self, depth: usize, new_len: u64) {
        self.branch += 1;
        let keep = self.canon.len() - depth;
        self.canon.truncate(keep);
        self.extend(new_len);
    }

    /// Makes an earlier branch canonical again.
    fn restore(&mut self, blocks: &[Blk]) {
        self.canon.truncate(blocks[0].number as usize);
        self.canon.extend_from_slice(blocks);
    }

    fn top(&self) -> u64 {
        self.canon.last().unwrap().number
    }

    fn head(&self) -> Blk {
        self.canon[self.lag.unwrap_or(self.top()) as usize]
    }
}

type Shared = Arc<Mutex<Chain>>;

fn json_block(b: &Blk) -> Value {
    json!({
        "number": format!("0x{:x}", b.number),
        "hash": b.hash,
        "parentHash": b.parent,
        "timestamp": format!("0x{:x}", b.ts),
        "transactions": [],
    })
}

async fn rpc_handler(
    State(chain): State<Shared>,
    Json(req): Json<Value>,
) -> axum::response::Response {
    let mut c = chain.lock().unwrap();
    match c.faults.pop_front().unwrap_or(Fault::Pass) {
        Fault::Pass => {}
        Fault::Http500 => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "upstream down").into_response();
        }
        Fault::Malformed => return (StatusCode::OK, "{not json").into_response(),
        Fault::RpcError => {
            return Json(json!({"jsonrpc": "2.0", "id": 1,
                "error": {"code": -32001, "message": "block unavailable"}}))
            .into_response();
        }
        Fault::NullResult => {
            return Json(json!({"jsonrpc": "2.0", "id": 1, "result": null})).into_response();
        }
        Fault::WrongBlock => {
            let b = c.canon[1];
            return Json(json!({"jsonrpc": "2.0", "id": 1, "result": json_block(&b)}))
                .into_response();
        }
    }
    let params = &req["params"];
    let result = match req["method"].as_str().unwrap_or_default() {
        "eth_chainId" => json!("0x97b1"),
        "eth_getBlockByNumber" => match params[0].as_str().unwrap() {
            "latest" => json_block(&c.head()),
            hex => {
                let n = u64::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap();
                c.canon
                    .get(n as usize)
                    .map(json_block)
                    .unwrap_or(Value::Null)
            }
        },
        "eth_getBlockByHash" => {
            let h: B256 = params[0].as_str().unwrap().parse().unwrap();
            c.by_hash.get(&h).map(json_block).unwrap_or(Value::Null)
        }
        m => {
            return Json(json!({"jsonrpc": "2.0", "id": 1,
                "error": {"code": -32601, "message": format!("no method {m}")}}))
            .into_response();
        }
    };
    Json(json!({"jsonrpc": "2.0", "id": req["id"], "result": result})).into_response()
}

async fn serve(chain: Shared) -> String {
    let app = Router::new()
        .route("/", post(rpc_handler))
        .with_state(chain);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

// ---------------------------------------------------------------------------
// Database fixtures
// ---------------------------------------------------------------------------

fn role_url() -> String {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let (scheme, rest) = url.split_once("://").expect("scheme");
    let host_and_db = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    format!("{scheme}://tidx_tipwatch:{ROLE_PASSWORD}@{host_and_db}")
}

struct Fixture {
    db: TestDb,
    /// Connected as tidx_tipwatch, like the deployed watcher.
    pool: Pool,
    chain: Shared,
    url: String,
}

async fn fixture(provider: &str, initial_blocks: u64) -> Fixture {
    let db = TestDb::empty().await;
    let su = db.pool.get().await.unwrap();
    su.batch_execute(store::ROLE_SQL)
        .await
        .expect("tipwatch_role.sql runs as the superuser");
    su.batch_execute(&format!(
        "ALTER ROLE tidx_tipwatch PASSWORD '{ROLE_PASSWORD}'"
    ))
    .await
    .unwrap();
    let pool = create_pool(&role_url())
        .await
        .expect("tidx_tipwatch can log in");
    store::ensure_schema(&pool)
        .await
        .expect("tidx_tipwatch creates its own tables");
    for sql in [
        "DELETE FROM tipwatch.orphans WHERE provider = $1",
        "UPDATE tipwatch.reorgs SET reverted_by = NULL WHERE provider = $1",
        "DELETE FROM tipwatch.reorgs WHERE provider = $1",
        "DELETE FROM tipwatch.seen WHERE provider = $1",
        "DELETE FROM tipwatch.gaps WHERE provider = $1",
    ] {
        su.execute(sql, &[&provider]).await.unwrap();
    }
    drop(su);

    let chain = Arc::new(Mutex::new(Chain::default()));
    chain.lock().unwrap().extend(initial_blocks);
    let url = serve(chain.clone()).await;
    Fixture {
        db,
        pool,
        chain,
        url,
    }
}

fn opts(memory_blocks: usize, max_walk: usize) -> WatchOptions {
    WatchOptions {
        memory_blocks,
        max_walk,
    }
}

async fn watcher(f: &Fixture, provider: &str, o: WatchOptions) -> Watcher {
    let rpc = TipRpc::new(&f.url, 1000.0).unwrap();
    Watcher::start(provider, rpc, f.pool.clone(), o)
        .await
        .unwrap()
}

#[derive(Debug, PartialEq)]
struct ReorgRow {
    id: i64,
    fork_point: i64,
    old_tip: i64,
    new_tip: i64,
    depth: i32,
    after_gap: bool,
    reverted_by: Option<i64>,
}

async fn reorgs(pool: &Pool, provider: &str) -> Vec<ReorgRow> {
    let c = pool.get().await.unwrap();
    c.query(
        "SELECT id, fork_point, old_tip, new_tip, depth, after_gap, reverted_by
         FROM tipwatch.reorgs WHERE provider = $1 ORDER BY id",
        &[&provider],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| ReorgRow {
        id: r.get(0),
        fork_point: r.get(1),
        old_tip: r.get(2),
        new_tip: r.get(3),
        depth: r.get(4),
        after_gap: r.get(5),
        reverted_by: r.get(6),
    })
    .collect()
}

async fn orphans(pool: &Pool, reorg_id: i64) -> Vec<(i64, B256)> {
    let c = pool.get().await.unwrap();
    c.query(
        "SELECT num, hash FROM tipwatch.orphans WHERE reorg_id = $1 ORDER BY num",
        &[&reorg_id],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), B256::from_slice(r.get::<_, &[u8]>(1))))
    .collect()
}

async fn seen(pool: &Pool, provider: &str) -> Vec<(i64, B256)> {
    let c = pool.get().await.unwrap();
    c.query(
        "SELECT num, hash FROM tipwatch.seen WHERE provider = $1 ORDER BY num",
        &[&provider],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), B256::from_slice(r.get::<_, &[u8]>(1))))
    .collect()
}

async fn gaps(pool: &Pool, provider: &str) -> Vec<(i64, i64, String)> {
    let c = pool.get().await.unwrap();
    c.query(
        "SELECT from_num, to_num, reason FROM tipwatch.gaps WHERE provider = $1 ORDER BY at, from_num",
        &[&provider],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), r.get(1), r.get(2)))
    .collect()
}

/// The provider's canonical hashes for the heights the watcher has stored.
fn canon_for(chain: &Shared, rows: &[(i64, B256)]) -> Vec<(i64, B256)> {
    let c = chain.lock().unwrap();
    rows.iter()
        .map(|(n, _)| (*n, c.canon[*n as usize].hash))
        .collect()
}

/// Steps once per block so the watcher remembers `n` more heights.
async fn grow(f: &Fixture, w: &mut Watcher, n: u64) {
    for _ in 0..n {
        f.chain.lock().unwrap().extend(1);
        assert!(matches!(
            w.step().await.unwrap(),
            StepOutcome::Extended { blocks: 1, .. }
        ));
    }
}

// ---------------------------------------------------------------------------
// Normal operation
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn extends_one_block_at_a_time_without_recording_a_reorg() {
    let p = "t-extend";
    let f = fixture(p, 10).await;
    let mut w = watcher(&f, p, opts(1000, 16)).await;

    assert_eq!(
        w.step().await.unwrap(),
        StepOutcome::Extended { blocks: 1, tip: 9 }
    );
    for tip in 10..15 {
        f.chain.lock().unwrap().extend(1);
        assert_eq!(
            w.step().await.unwrap(),
            StepOutcome::Extended { blocks: 1, tip }
        );
    }
    assert_eq!(w.step().await.unwrap(), StepOutcome::Unchanged);

    let rows = seen(&f.db.pool, p).await;
    assert_eq!(
        rows.iter().map(|r| r.0).collect::<Vec<_>>(),
        (9..=14).collect::<Vec<_>>()
    );
    assert_eq!(rows, canon_for(&f.chain, &rows));
    assert!(reorgs(&f.db.pool, p).await.is_empty());
    assert_eq!(w.tip().unwrap().number, 14);
}

#[tokio::test]
#[serial]
async fn fills_skipped_heights_by_walking_parent_hashes() {
    let p = "t-skip";
    let f = fixture(p, 5).await;
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    w.step().await.unwrap();

    f.chain.lock().unwrap().extend(6);
    assert_eq!(
        w.step().await.unwrap(),
        StepOutcome::Extended { blocks: 6, tip: 10 }
    );

    let rows = seen(&f.db.pool, p).await;
    assert_eq!(
        rows.iter().map(|r| r.0).collect::<Vec<_>>(),
        (4..=10).collect::<Vec<_>>()
    );
    assert_eq!(rows, canon_for(&f.chain, &rows));
    assert!(reorgs(&f.db.pool, p).await.is_empty());
}

#[tokio::test]
#[serial]
async fn lagging_backend_answer_is_not_a_reorg() {
    let p = "t-lag";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    w.step().await.unwrap();
    grow(&f, &mut w, 6).await;

    f.chain.lock().unwrap().lag = Some(5);
    assert_eq!(w.step().await.unwrap(), StepOutcome::Unchanged);
    f.chain.lock().unwrap().lag = None;
    f.chain.lock().unwrap().extend(1);
    assert_eq!(
        w.step().await.unwrap(),
        StepOutcome::Extended { blocks: 1, tip: 9 }
    );
    assert!(reorgs(&f.db.pool, p).await.is_empty());
}

// ---------------------------------------------------------------------------
// Reorgs
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn records_a_one_block_reorg_with_its_orphan() {
    let p = "t-reorg1";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    w.step().await.unwrap();
    grow(&f, &mut w, 3).await;
    let old_top = *f.chain.lock().unwrap().canon.last().unwrap();

    f.chain.lock().unwrap().reorg(1, 1);
    let out = w.step().await.unwrap();
    let rows = reorgs(&f.db.pool, p).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        out,
        StepOutcome::Reorg {
            id: rows[0].id,
            depth: 1,
            fork_point: 4
        }
    );
    assert_eq!(
        rows[0],
        ReorgRow {
            id: rows[0].id,
            fork_point: 4,
            old_tip: 5,
            new_tip: 5,
            depth: 1,
            after_gap: false,
            reverted_by: None
        }
    );
    assert_eq!(
        orphans(&f.db.pool, rows[0].id).await,
        vec![(5, old_top.hash)]
    );

    let s = seen(&f.db.pool, p).await;
    assert_eq!(s, canon_for(&f.chain, &s), "seen follows the new branch");
}

#[tokio::test]
#[serial]
async fn records_a_reorg_deeper_than_the_engine_head_delay() {
    let p = "t-reorg40";
    let f = fixture(p, 1).await;
    let mut w = watcher(&f, p, opts(1000, 64)).await;
    w.step().await.unwrap();
    f.chain.lock().unwrap().extend(50);
    assert_eq!(
        w.step().await.unwrap(),
        StepOutcome::Extended {
            blocks: 50,
            tip: 50
        }
    );

    f.chain.lock().unwrap().reorg(40, 42);
    let out = w.step().await.unwrap();
    let rows = reorgs(&f.db.pool, p).await;
    assert_eq!(
        out,
        StepOutcome::Reorg {
            id: rows[0].id,
            depth: 40,
            fork_point: 10
        }
    );
    assert_eq!(
        (rows[0].old_tip, rows[0].new_tip, rows[0].depth),
        (50, 52, 40)
    );
    assert_eq!(orphans(&f.db.pool, rows[0].id).await.len(), 40);
    let s = seen(&f.db.pool, p).await;
    assert_eq!(s.last().unwrap().0, 52);
    assert_eq!(s, canon_for(&f.chain, &s));
}

#[tokio::test]
#[serial]
async fn reorg_to_a_shorter_branch_counts_every_replaced_block() {
    let p = "t-shorter";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    w.step().await.unwrap();
    grow(&f, &mut w, 10).await;

    f.chain.lock().unwrap().reorg(5, 3);
    let out = w.step().await.unwrap();
    let rows = reorgs(&f.db.pool, p).await;
    assert_eq!(
        out,
        StepOutcome::Reorg {
            id: rows[0].id,
            depth: 5,
            fork_point: 7
        }
    );
    assert_eq!((rows[0].old_tip, rows[0].new_tip), (12, 10));
    let s = seen(&f.db.pool, p).await;
    assert_eq!(
        s.last().unwrap().0,
        10,
        "heights above the new tip are gone from seen"
    );
    assert_eq!(s, canon_for(&f.chain, &s));
}

#[tokio::test]
#[serial]
async fn switching_back_to_an_orphaned_branch_marks_the_first_reorg_reverted() {
    let p = "t-flap";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    w.step().await.unwrap();
    grow(&f, &mut w, 5).await;
    let old_branch: Vec<Blk> = f.chain.lock().unwrap().canon[6..=7].to_vec();

    f.chain.lock().unwrap().reorg(2, 2);
    w.step().await.unwrap();
    f.chain.lock().unwrap().restore(&old_branch);
    w.step().await.unwrap();

    let rows = reorgs(&f.db.pool, p).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].reverted_by, Some(rows[1].id));
    assert_eq!(rows[1].reverted_by, None);
}

// ---------------------------------------------------------------------------
// Failure modes
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn rpc_failures_return_errors_and_leave_state_untouched() {
    let p = "t-faults";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    w.step().await.unwrap();
    grow(&f, &mut w, 2).await;
    let before = seen(&f.db.pool, p).await;

    for (fault, expect) in [
        (Fault::Http500, "HTTP 500"),
        (Fault::Malformed, "invalid JSON"),
        (Fault::RpcError, "-32001"),
        (Fault::NullResult, "no block"),
    ] {
        f.chain.lock().unwrap().faults.push_back(fault);
        let err = w.step().await.expect_err("fault must surface as an error");
        assert!(format!("{err:#}").contains(expect), "{fault:?}: {err:#}");
    }

    // A parent fetched by hash that is some other block.
    f.chain.lock().unwrap().extend(3);
    f.chain
        .lock()
        .unwrap()
        .faults
        .extend([Fault::Pass, Fault::WrongBlock]);
    let err = w
        .step()
        .await
        .expect_err("mismatched parent must be rejected");
    assert!(format!("{err:#}").contains("asked for"), "{err:#}");

    assert_eq!(
        seen(&f.db.pool, p).await,
        before,
        "failed steps write nothing"
    );
    assert_eq!(
        w.step().await.unwrap(),
        StepOutcome::Extended { blocks: 3, tip: 7 }
    );
    assert!(reorgs(&f.db.pool, p).await.is_empty());
}

// ---------------------------------------------------------------------------
// Restarts and blind periods
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn restart_finds_a_reorg_that_happened_while_stopped() {
    let p = "t-restart-short";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    w.step().await.unwrap();
    grow(&f, &mut w, 5).await;
    drop(w);

    f.chain.lock().unwrap().reorg(2, 2);
    f.chain.lock().unwrap().extend(5);
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    assert_eq!(w.tip().unwrap().number, 7, "reloaded from tipwatch.seen");

    let out = w.step().await.unwrap();
    let rows = reorgs(&f.db.pool, p).await;
    assert_eq!(
        out,
        StepOutcome::Reorg {
            id: rows[0].id,
            depth: 2,
            fork_point: 5
        }
    );
    assert!(
        !rows[0].after_gap,
        "found by parent hashes, every height observed"
    );
    assert!(gaps(&f.db.pool, p).await.is_empty());
    let s = seen(&f.db.pool, p).await;
    assert_eq!(s, canon_for(&f.chain, &s));
}

#[tokio::test]
#[serial]
async fn long_stop_compares_by_number_and_records_the_gap() {
    let p = "t-restart-long";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    w.step().await.unwrap();
    grow(&f, &mut w, 7).await; // tip 9
    drop(w);

    f.chain.lock().unwrap().reorg(3, 3); // heights 7..=9 replaced
    f.chain.lock().unwrap().extend(40); // tip 49, beyond max_walk
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    let out = w.step().await.unwrap();
    let rows = reorgs(&f.db.pool, p).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        out,
        StepOutcome::Jump {
            reorg: Some((rows[0].id, 3)),
            gap_from: 10,
            gap_to: 48
        }
    );
    assert_eq!(
        (
            rows[0].fork_point,
            rows[0].old_tip,
            rows[0].new_tip,
            rows[0].depth,
            rows[0].after_gap
        ),
        (6, 9, 9, 3, true)
    );
    let g = gaps(&f.db.pool, p).await;
    assert_eq!(g.len(), 1);
    assert_eq!((g[0].0, g[0].1), (10, 48));

    f.chain.lock().unwrap().extend(1);
    assert_eq!(
        w.step().await.unwrap(),
        StepOutcome::Extended { blocks: 1, tip: 50 }
    );
    let s = seen(&f.db.pool, p).await;
    assert_eq!(s, canon_for(&f.chain, &s));
}

#[tokio::test]
#[serial]
async fn long_stop_without_a_reorg_records_only_the_gap() {
    let p = "t-gap-only";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    w.step().await.unwrap();
    grow(&f, &mut w, 3).await; // tip 5
    drop(w);

    f.chain.lock().unwrap().extend(40); // tip 45
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    assert_eq!(
        w.step().await.unwrap(),
        StepOutcome::Jump {
            reorg: None,
            gap_from: 6,
            gap_to: 44
        }
    );
    assert!(reorgs(&f.db.pool, p).await.is_empty());
    assert_eq!(gaps(&f.db.pool, p).await.len(), 1);
}

#[tokio::test]
#[serial]
async fn divergence_deeper_than_memory_is_a_gap_not_a_guess() {
    let p = "t-diverge";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(8, 4)).await;
    w.step().await.unwrap();
    grow(&f, &mut w, 10).await; // tip 12, memory 5..=12

    f.chain.lock().unwrap().reorg(10, 12); // fork at 2, below memory; tip 14
    let out = w.step().await.unwrap();
    assert_eq!(
        out,
        StepOutcome::Diverged {
            gap_from: 5,
            gap_to: 13
        }
    );
    assert!(reorgs(&f.db.pool, p).await.is_empty());
    let g = gaps(&f.db.pool, p).await;
    assert!(g[0].2.contains("diverged"), "{g:?}");

    f.chain.lock().unwrap().extend(1);
    assert_eq!(
        w.step().await.unwrap(),
        StepOutcome::Extended { blocks: 1, tip: 15 }
    );
}

// ---------------------------------------------------------------------------
// Concurrency, pacing, retention, privileges
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn providers_are_tracked_independently() {
    let (pa, pb) = ("t-prov-a", "t-prov-b");
    let f = fixture(pa, 3).await;
    let _ = fixture_cleanup(&f, pb).await;
    let chain_b = Arc::new(Mutex::new(Chain::with_branch(1000)));
    chain_b.lock().unwrap().extend(3);
    let url_b = serve(chain_b.clone()).await;

    let mut wa = watcher(&f, pa, opts(1000, 16)).await;
    let mut wb = Watcher::start(
        pb,
        TipRpc::new(&url_b, 1000.0).unwrap(),
        f.pool.clone(),
        opts(1000, 16),
    )
    .await
    .unwrap();
    for round in 0..6 {
        f.chain.lock().unwrap().extend(1);
        chain_b.lock().unwrap().extend(1);
        if round == 3 {
            f.chain.lock().unwrap().reorg(2, 2);
        }
        let (a, b) = tokio::join!(wa.step(), wb.step());
        a.unwrap();
        b.unwrap();
    }

    assert_eq!(reorgs(&f.db.pool, pa).await.len(), 1);
    assert!(
        reorgs(&f.db.pool, pb).await.is_empty(),
        "the two chains differ, but that is not a reorg"
    );
    let sa = seen(&f.db.pool, pa).await;
    let sb = seen(&f.db.pool, pb).await;
    assert_eq!(sa, canon_for(&f.chain, &sa));
    assert_eq!(sb, canon_for(&chain_b, &sb));
}

async fn fixture_cleanup(f: &Fixture, provider: &str) {
    let c = f.db.pool.get().await.unwrap();
    for sql in [
        "DELETE FROM tipwatch.orphans WHERE provider = $1",
        "UPDATE tipwatch.reorgs SET reverted_by = NULL WHERE provider = $1",
        "DELETE FROM tipwatch.reorgs WHERE provider = $1",
        "DELETE FROM tipwatch.seen WHERE provider = $1",
        "DELETE FROM tipwatch.gaps WHERE provider = $1",
    ] {
        c.execute(sql, &[&provider]).await.unwrap();
    }
}

#[tokio::test]
#[serial]
async fn requests_are_paced_to_max_rps() {
    let f = fixture("t-pace", 3).await;
    let rpc = Arc::new(TipRpc::new(&f.url, 10.0).unwrap());
    assert_eq!(rpc.chain_id().await.unwrap(), 38833);

    let start = Instant::now();
    for _ in 0..5 {
        rpc.head().await.unwrap();
    }
    let sequential = start.elapsed();
    assert!(sequential >= Duration::from_millis(450), "{sequential:?}");

    let start = Instant::now();
    let calls: Vec<_> = (0..6)
        .map(|_| {
            let r = rpc.clone();
            tokio::spawn(async move { r.head().await })
        })
        .collect();
    for c in calls {
        c.await.unwrap().unwrap();
    }
    let concurrent = start.elapsed();
    assert!(
        concurrent >= Duration::from_millis(500),
        "concurrent callers share one budget: {concurrent:?}"
    );
    assert!(concurrent < Duration::from_secs(3), "{concurrent:?}");

    assert!(
        TipRpc::new(&f.url, 0.0).is_err(),
        "a zero rate would never send"
    );
}

#[tokio::test]
#[serial]
async fn prune_keeps_only_the_memory_window() {
    let p = "t-prune";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(20, 64)).await;
    w.step().await.unwrap();
    for _ in 0..6 {
        f.chain.lock().unwrap().extend(10);
        w.step().await.unwrap();
    }
    let tip = w.tip().unwrap().number as i64;
    w.prune().await.unwrap();
    let s = seen(&f.db.pool, p).await;
    assert_eq!(s.len(), 20);
    assert_eq!(s.first().unwrap().0, tip - 19);
    assert_eq!(s.last().unwrap().0, tip);
}

#[tokio::test]
#[serial]
async fn watcher_role_cannot_touch_the_indexed_tables() {
    let f = fixture("t-privs", 3).await;
    let c = f.pool.get().await.unwrap();
    for sql in [
        "SELECT count(*) FROM public.blocks",
        "DELETE FROM public.sync_state",
        "CREATE TABLE public.tipwatch_probe (x int)",
        "SELECT pg_read_file('/etc/passwd')",
    ] {
        let err = c.batch_execute(sql).await.expect_err(sql);
        assert_eq!(
            err.code().map(|s| s.code()),
            Some(INSUFFICIENT_PRIVILEGE),
            "{sql}: {err}"
        );
    }
    c.batch_execute("SELECT count(*) FROM tipwatch.reorgs")
        .await
        .unwrap();
    store::ensure_schema(&f.pool).await.expect("idempotent");
}
