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

#[derive(Clone, Copy, Debug)]
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
    /// Answer the next request with this block, whatever was asked.
    Serve(Blk),
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
        Fault::Serve(b) => {
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
        orphan_retention_days: None,
        stall_after: Duration::from_millis(300),
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

#[tokio::test]
#[serial]
async fn one_block_flip_right_after_start_is_a_reorg_not_a_divergence() {
    // Right after start (or after a gap) only the head is remembered, and Igra
    // flips its tip block every few seconds.
    let p = "t-flip-1";
    let f = fixture(p, 5).await;
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    w.step().await.unwrap(); // memory: block 4 only
    let old = f.chain.lock().unwrap().canon[4];

    f.chain.lock().unwrap().reorg(1, 1);
    let out = w.step().await.unwrap();
    let rows = reorgs(&f.db.pool, p).await;
    assert_eq!(
        out,
        StepOutcome::Reorg {
            id: rows[0].id,
            depth: 1,
            fork_point: 3
        }
    );
    assert_eq!(orphans(&f.db.pool, rows[0].id).await, vec![(4, old.hash)]);
    assert!(gaps(&f.db.pool, p).await.is_empty());

    // The same with a head that also moved on.
    f.chain.lock().unwrap().reorg(1, 3);
    assert!(matches!(
        w.step().await.unwrap(),
        StepOutcome::Reorg {
            depth: 1,
            fork_point: 3,
            ..
        }
    ));
    assert!(gaps(&f.db.pool, p).await.is_empty());
}

#[tokio::test]
#[serial]
async fn reorg_deeper_than_max_walk_is_measured_by_a_number_search() {
    let p = "t-deep-walk";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(1000, 2)).await;
    w.step().await.unwrap();
    grow(&f, &mut w, 7).await; // tip 9

    let old: Vec<Blk> = f.chain.lock().unwrap().canon[7..=9].to_vec();
    f.chain.lock().unwrap().reorg(3, 4); // 7..=9 replaced, head 10
    let out = w.step().await.unwrap();
    let rows = reorgs(&f.db.pool, p).await;
    // Exact fork and depth from memory; the new branch is longer than
    // max_walk, so only its first block (7) is fetched and 8..=9 are a gap.
    assert_eq!(
        out,
        StepOutcome::Jump {
            reorg: Some((rows[0].id, 3)),
            gap_from: 8,
            gap_to: 9
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
        (6, 9, 10, 3, true)
    );
    assert_eq!(
        orphans(&f.db.pool, rows[0].id).await,
        old.iter()
            .map(|b| (b.number as i64, b.hash))
            .collect::<Vec<_>>()
    );

    f.chain.lock().unwrap().extend(1);
    assert_eq!(
        w.step().await.unwrap(),
        StepOutcome::Extended { blocks: 1, tip: 11 }
    );
}

#[tokio::test]
#[serial]
async fn by_number_blocks_that_do_not_link_are_retried_not_recorded() {
    let p = "t-mid-change";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    w.step().await.unwrap();
    grow(&f, &mut w, 7).await; // tip 9, memory 2..=9

    let other = {
        let mut c = f.chain.lock().unwrap();
        c.reorg(3, 3); // 7..=9 replaced
        c.extend(40); // head 49: compared by number straight away
        let other = Chain::with_branch(9_999).mk(8, B256::repeat_byte(7));
        // head; probes at 9, 2, 5, 7, 6 find the fork at 6; then the new
        // branch is fetched: 7 is fine, 8 comes from yet another branch.
        c.faults.extend([Fault::Pass; 7]);
        c.faults.push_back(Fault::Serve(other));
        other
    };
    let err = w
        .step()
        .await
        .expect_err("unlinked blocks must not be recorded");
    assert!(
        format!("{err:#}").contains("changed while being compared"),
        "{err:#}"
    );
    assert!(reorgs(&f.db.pool, p).await.is_empty());
    assert!(gaps(&f.db.pool, p).await.is_empty());
    assert!(
        seen(&f.db.pool, p)
            .await
            .iter()
            .all(|(_, h)| *h != other.hash)
    );

    let out = w.step().await.unwrap();
    assert!(
        matches!(
            out,
            StepOutcome::Jump {
                reorg: Some((_, 3)),
                gap_from: 10,
                gap_to: 48
            }
        ),
        "{out:?}"
    );
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
        (6, 9, 49, 3, true)
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
async fn divergence_to_a_lower_head_leaves_no_stale_rows_for_a_restart() {
    let p = "t-diverge-low";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(8, 4)).await;
    w.step().await.unwrap();
    grow(&f, &mut w, 10).await; // tip 12, memory 5..=12

    f.chain.lock().unwrap().reorg(10, 8); // fork at 2, below memory; head 10 < old tip
    assert_eq!(
        w.step().await.unwrap(),
        StepOutcome::Diverged {
            gap_from: 5,
            gap_to: 12
        }
    );
    let s = seen(&f.db.pool, p).await;
    assert_eq!(
        s.iter().map(|r| r.0).collect::<Vec<_>>(),
        vec![10],
        "only the head is remembered"
    );
    drop(w);

    let mut w = watcher(&f, p, opts(8, 4)).await;
    assert_eq!(
        w.tip().unwrap().number,
        10,
        "a restart resumes from the live head, not the dead branch"
    );
    f.chain.lock().unwrap().extend(1);
    assert_eq!(
        w.step().await.unwrap(),
        StepOutcome::Extended { blocks: 1, tip: 11 }
    );
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
async fn prune_drops_old_orphan_copies_but_keeps_every_reorg_event() {
    let p = "t-retention";
    let f = fixture(p, 3).await;
    let mut w = watcher(&f, p, opts(1000, 16)).await;
    w.step().await.unwrap();
    grow(&f, &mut w, 3).await;
    for _ in 0..2 {
        f.chain.lock().unwrap().reorg(1, 1);
        assert!(matches!(w.step().await.unwrap(), StepOutcome::Reorg { .. }));
    }
    let rows = reorgs(&f.db.pool, p).await;
    let su = f.db.pool.get().await.unwrap();
    su.execute(
        "UPDATE tipwatch.reorgs SET detected_at = now() - interval '91 days' WHERE id = $1",
        &[&rows[0].id],
    )
    .await
    .unwrap();

    // Without a retention nothing is dropped.
    w.prune().await.unwrap();
    assert_eq!(orphans(&f.db.pool, rows[0].id).await.len(), 1);
    drop(w);

    let o = WatchOptions {
        orphan_retention_days: Some(90),
        ..opts(1000, 16)
    };
    let w = watcher(&f, p, o).await;
    w.prune().await.unwrap();
    assert!(
        orphans(&f.db.pool, rows[0].id).await.is_empty(),
        "older than 90 days: copies dropped"
    );
    assert_eq!(
        orphans(&f.db.pool, rows[1].id).await.len(),
        1,
        "recent copies kept"
    );
    assert_eq!(
        reorgs(&f.db.pool, p).await,
        rows,
        "reorg events are kept forever"
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

// ---------------------------------------------------------------------------
// Pushed heads (eth_subscribe newHeads)
// ---------------------------------------------------------------------------
//
// Polling misses tip versions that live shorter than one poll: on 2026-11-16 the
// own node pushed 23 versions in 2 minutes that were later dropped, and the
// 4-requests/s poller recorded 14 of them.

#[derive(Clone, Copy, PartialEq)]
enum WsMode {
    Normal,
    DropAfterSubscribe,
    Silent,
    Refuse,
}

struct WsServer {
    url: String,
    pushes: tokio::sync::broadcast::Sender<String>,
    subscriptions: Arc<std::sync::atomic::AtomicUsize>,
}

impl WsServer {
    async fn start(mode: WsMode) -> Self {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (pushes, _) = tokio::sync::broadcast::channel::<String>(1024);
        let subscriptions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mode = Arc::new(Mutex::new(mode));
        let (tx, subs, m) = (pushes.clone(), subscriptions.clone(), mode.clone());
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (tx, subs, m) = (tx.clone(), subs.clone(), m.clone());
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                        return;
                    };
                    let Some(Ok(Message::Text(_subscribe))) = ws.next().await else {
                        return;
                    };
                    let mode = *m.lock().unwrap();
                    if mode == WsMode::Refuse {
                        let _ = ws
                            .send(Message::text(
                                json!({"jsonrpc": "2.0", "id": 1,
                                "error": {"code": -32601, "message": "subscriptions disabled"}})
                                .to_string(),
                            ))
                            .await;
                        return;
                    }
                    let mut rx = tx.subscribe();
                    let _ = ws
                        .send(Message::text(
                            json!({"jsonrpc": "2.0", "id": 1, "result": "0xsub"}).to_string(),
                        ))
                        .await;
                    subs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    match mode {
                        WsMode::DropAfterSubscribe => {
                            *m.lock().unwrap() = WsMode::Normal;
                        }
                        WsMode::Silent => {
                            *m.lock().unwrap() = WsMode::Normal;
                            while ws.next().await.is_some() {}
                        }
                        _ => {
                            while let Ok(text) = rx.recv().await {
                                if ws.send(Message::text(text)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
        });
        Self {
            url,
            pushes,
            subscriptions,
        }
    }

    fn push(&self, b: &Blk) {
        self.push_raw(
            json!({"jsonrpc": "2.0", "method": "eth_subscription",
                   "params": {"subscription": "0xsub", "result": json_block(b)}})
            .to_string(),
        );
    }

    fn push_raw(&self, text: String) {
        let _ = self.pushes.send(text);
    }

    fn subscriptions(&self) -> usize {
        self.subscriptions.load(std::sync::atomic::Ordering::SeqCst)
    }
}

async fn wait_for<F, Fut>(what: &str, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(15);
    while !cond().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn seen_tip(pool: &Pool, provider: &str) -> i64 {
    let c = pool.get().await.unwrap();
    c.query_one(
        "SELECT coalesce(max(num), -1) FROM tipwatch.seen WHERE provider = $1",
        &[&provider],
    )
    .await
    .unwrap()
    .get(0)
}

/// `stall_after` short: quiet subscriptions get replaced quickly. Long: the
/// subscription count stays stable while nothing is pushed.
async fn run_with_ws(
    f: &Fixture,
    provider: &str,
    ws: &WsServer,
    stall_after: Duration,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let o = WatchOptions {
        stall_after,
        ..opts(1000, 16)
    };
    let w = watcher(f, provider, o).await.with_ws(&ws.url);
    tokio::spawn(w.run())
}

#[tokio::test]
#[serial]
async fn pushed_versions_that_polling_never_sees_are_recorded() {
    let p = "t-push";
    let f = fixture(p, 5).await;
    // HTTP keeps answering with block 4, so only the push stream shows the tip.
    f.chain.lock().unwrap().lag = Some(4);
    let ws = WsServer::start(WsMode::Normal).await;
    let task = run_with_ws(&f, p, &ws, Duration::from_secs(30)).await;
    wait_for("the subscription", || async { ws.subscriptions() == 1 }).await;
    wait_for("the catch-up poll", || async {
        seen_tip(&f.db.pool, p).await == 4
    })
    .await;

    let (b5, b5x, b5y) = {
        let mut c = f.chain.lock().unwrap();
        c.extend(1);
        let b5 = c.canon[5];
        c.reorg(1, 1);
        let b5x = c.canon[5];
        c.reorg(1, 1);
        (b5, b5x, c.canon[5])
    };
    for b in [b5, b5x, b5y] {
        ws.push(&b);
    }
    wait_for("two reorgs", || async {
        reorgs(&f.db.pool, p).await.len() == 2
    })
    .await;
    task.abort();

    let rows = reorgs(&f.db.pool, p).await;
    assert_eq!(orphans(&f.db.pool, rows[0].id).await, vec![(5, b5.hash)]);
    assert_eq!(orphans(&f.db.pool, rows[1].id).await, vec![(5, b5x.hash)]);
    assert_eq!(seen(&f.db.pool, p).await.last().unwrap(), &(5, b5y.hash));
}

#[tokio::test]
#[serial]
async fn dropped_subscription_falls_back_to_polling_and_resubscribes() {
    let p = "t-push-drop";
    let f = fixture(p, 5).await;
    let ws = WsServer::start(WsMode::DropAfterSubscribe).await;
    let task = run_with_ws(&f, p, &ws, Duration::from_millis(300)).await;
    wait_for("the first subscription", || async {
        ws.subscriptions() >= 1
    })
    .await;

    f.chain.lock().unwrap().extend(3); // only visible by polling while unsubscribed
    wait_for("polling to reach block 7", || async {
        seen_tip(&f.db.pool, p).await == 7
    })
    .await;
    wait_for("a second subscription", || async {
        ws.subscriptions() >= 2
    })
    .await;

    let b8 = {
        let mut c = f.chain.lock().unwrap();
        c.extend(1);
        c.canon[8]
    };
    ws.push(&b8);
    wait_for("block 8", || async { seen_tip(&f.db.pool, p).await == 8 }).await;
    task.abort();
    let s = seen(&f.db.pool, p).await;
    assert_eq!(s, canon_for(&f.chain, &s));
    assert!(gaps(&f.db.pool, p).await.is_empty());
}

#[tokio::test]
#[serial]
async fn silent_subscription_is_replaced_and_polling_covers_the_gap() {
    let p = "t-push-silent";
    let f = fixture(p, 5).await;
    let ws = WsServer::start(WsMode::Silent).await;
    let task = run_with_ws(&f, p, &ws, Duration::from_millis(300)).await;
    wait_for("the first subscription", || async {
        ws.subscriptions() >= 1
    })
    .await;

    f.chain.lock().unwrap().extend(2);
    wait_for("polling to reach block 6", || async {
        seen_tip(&f.db.pool, p).await == 6
    })
    .await;
    wait_for("a fresh subscription", || async { ws.subscriptions() >= 2 }).await;
    task.abort();
}

#[tokio::test]
#[serial]
async fn refused_subscription_falls_back_to_polling() {
    let p = "t-push-refused";
    let f = fixture(p, 5).await;
    let ws = WsServer::start(WsMode::Refuse).await;
    let task = run_with_ws(&f, p, &ws, Duration::from_millis(300)).await;

    f.chain.lock().unwrap().extend(2);
    wait_for("polling to reach block 6", || async {
        seen_tip(&f.db.pool, p).await == 6
    })
    .await;
    assert_eq!(ws.subscriptions(), 0);
    assert!(!task.is_finished(), "the watcher keeps running");
    task.abort();
}

#[tokio::test]
#[serial]
async fn malformed_pushes_are_ignored() {
    let p = "t-push-garbage";
    let f = fixture(p, 5).await;
    f.chain.lock().unwrap().lag = Some(4);
    let ws = WsServer::start(WsMode::Normal).await;
    let task = run_with_ws(&f, p, &ws, Duration::from_secs(30)).await;
    wait_for("the subscription", || async { ws.subscriptions() == 1 }).await;
    wait_for("the catch-up poll", || async {
        seen_tip(&f.db.pool, p).await == 4
    })
    .await;

    let b5 = {
        let mut c = f.chain.lock().unwrap();
        c.extend(1);
        c.canon[5]
    };
    for junk in [
        "{not json".to_string(),
        json!({"jsonrpc": "2.0", "method": "eth_subscription",
               "params": {"subscription": "0xother", "result": json_block(&b5)}})
        .to_string(),
        json!({"jsonrpc": "2.0", "method": "eth_subscription",
               "params": {"subscription": "0xsub", "result": {"number": "0x5"}}})
        .to_string(),
        json!({"jsonrpc": "2.0", "method": "something_else", "params": {}}).to_string(),
    ] {
        ws.push_raw(junk);
    }
    ws.push(&b5);
    wait_for("block 5", || async { seen_tip(&f.db.pool, p).await == 5 }).await;
    assert!(!task.is_finished());
    task.abort();
    assert!(reorgs(&f.db.pool, p).await.is_empty());
    assert_eq!(ws.subscriptions(), 1, "junk did not cost the subscription");
}
