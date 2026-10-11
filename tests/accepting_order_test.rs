//! Kaspa acceptance order on final provenance rows. An L2 tx can have several
//! L1 carriers; consumers need the accepting block's blue score to order
//! them. Sender enrichment stores it from the same api.kaspa.org batch
//! response, including on rows enriched before the columns existed.
//!
//! Requires `DATABASE_URL` → `tidx-test-pg`.

mod common;

use axum::{Json, Router, extract::State, routing::post};
use common::testdb::TestDb;
use serde_json::{Value, json};
use serial_test::serial;
use std::sync::Arc;
use tidx::db::Pool;
use tidx::kaspa::enrichment::enrich_table;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
struct Fake {
    requested: Arc<Mutex<Vec<String>>>,
}

/// Answers for every requested txid except `…ee` (unknown to the API).
async fn search(State(f): State<Fake>, Json(body): Json<Value>) -> Json<Value> {
    let ids: Vec<String> = body["transactionIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    f.requested.lock().await.extend(ids.clone());
    let txs: Vec<Value> = ids
        .iter()
        .filter(|id| !id.ends_with("ee"))
        .map(|id| {
            let tag = u8::from_str_radix(&id[..2], 16).unwrap() as i64;
            json!({
                "transaction_id": id,
                "inputs": [{"previous_outpoint_address": "kaspa:fresh", "previous_outpoint_amount": 1000}],
                "outputs": [{"amount": 900}],
                "accepting_block_hash": format!("{:02x}", tag).repeat(32),
                "accepting_block_blue_score": 561_000_000 + tag,
            })
        })
        .collect();
    Json(Value::Array(txs))
}

async fn start_fake() -> (String, Fake) {
    let fake = Fake::default();
    let app = Router::new()
        .route("/transactions/search", post(search))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://127.0.0.1:{}", addr.port()), fake)
}

fn txid(tag: u8) -> Vec<u8> {
    let mut v = vec![tag; 32];
    v[31] = tag;
    v
}

/// Seeds: L2 0xaa carried by 0x11 + 0x12 (multi-carrier, already enriched
/// before the accepting columns existed); L2 0xbb carried by 0x21 only (not
/// enriched); L2 0xcc carried by 0x31 + 0xee (0xee unknown to the API).
async fn seed(pool: &Pool) {
    let conn = pool.get().await.unwrap();
    conn.batch_execute("TRUNCATE kaspa_l2_submissions, kaspa_entries").await.unwrap();
    for (l2, carrier, enriched) in [
        (0xaa, 0x11, true),
        (0xaa, 0x12, true),
        (0xbb, 0x21, false),
        (0xcc, 0x31, false),
        (0xcc, 0xee, false),
    ] {
        conn.execute(
            "INSERT INTO kaspa_l2_submissions (l2_tx_hash, kaspa_txid, l1_senders, l1_sender_amounts_sompi, l1_enriched_at, l1_fee_sompi, l1_fee_enriched_at)
             VALUES ($1, $2, $3, $4, CASE WHEN $5 THEN now() END, $6, CASE WHEN $5 THEN now() END)",
            &[
                &vec![l2 as u8; 32],
                &txid(carrier),
                &enriched.then(|| vec!["kaspa:original".to_string()]),
                &enriched.then(|| vec![5_i64]),
                &enriched,
                &enriched.then_some(7_i64),
            ],
        )
        .await
        .unwrap();
    }
}

async fn accepting(pool: &Pool, carrier: u8) -> (Option<Vec<u8>>, Option<i64>, Option<Vec<String>>) {
    let row = pool
        .get()
        .await
        .unwrap()
        .query_one(
            "SELECT accepting_block_hash, accepting_blue_score, l1_senders FROM kaspa_l2_submissions WHERE kaspa_txid = $1",
            &[&txid(carrier)],
        )
        .await
        .unwrap();
    (row.get(0), row.get(1), row.get(2))
}

#[tokio::test]
#[serial(db)]
async fn final_tables_have_accepting_columns() {
    let db = TestDb::empty().await;
    let conn = db.pool.get().await.unwrap();
    for table in ["kaspa_l2_submissions", "kaspa_entries"] {
        let cols: Vec<String> = conn
            .query(
                "SELECT column_name || ':' || udt_name FROM information_schema.columns
                 WHERE table_name = $1 AND column_name LIKE 'accepting_%' ORDER BY 1",
                &[&table],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(cols, vec!["accepting_block_hash:bytea", "accepting_blue_score:int8"], "{table}");
    }
    // Enrichment pages newest-first on (created_at, kaspa_txid); without this
    // index every 500-row batch is a full scan of the table.
    for index in ["idx_kaspa_l2_submissions_created_at", "idx_kaspa_entries_created_at"] {
        let def: Option<String> = conn
            .query_opt("SELECT indexdef FROM pg_indexes WHERE indexname = $1", &[&index])
            .await
            .unwrap()
            .map(|r| r.get(0));
        assert!(def.as_deref().unwrap_or("").contains("(created_at, kaspa_txid)"), "{index}: {def:?}");
    }
}

#[tokio::test]
#[serial(db)]
async fn enrichment_backfills_acceptance_without_touching_existing_senders() {
    let db = TestDb::empty().await;
    seed(&db.pool).await;
    let (base, _) = start_fake().await;

    enrich_table(&db.pool, &reqwest::Client::new(), &base, "kaspa_l2_submissions", 1, 500, None, false)
        .await
        .unwrap();

    let (hash, score, senders) = accepting(&db.pool, 0x11).await;
    assert_eq!(hash, Some(vec![0x11; 32]));
    assert_eq!(score, Some(561_000_017));
    assert_eq!(senders, Some(vec!["kaspa:original".into()]), "existing senders are not overwritten");
    assert_eq!(accepting(&db.pool, 0x12).await.1, Some(561_000_018));

    let (_, score, senders) = accepting(&db.pool, 0x21).await;
    assert_eq!(score, Some(561_000_033));
    assert_eq!(senders, Some(vec!["kaspa:fresh".into()]), "new rows still get senders");

    assert_eq!(accepting(&db.pool, 0xee).await.1, None, "API miss stays NULL for the next run");
}

#[tokio::test]
#[serial(db)]
async fn multi_carrier_only_requests_just_the_duplicate_groups() {
    let db = TestDb::empty().await;
    seed(&db.pool).await;
    let (base, fake) = start_fake().await;

    enrich_table(&db.pool, &reqwest::Client::new(), &base, "kaspa_l2_submissions", 1, 500, None, true)
        .await
        .unwrap();

    let mut requested = fake.requested.lock().await.clone();
    requested.sort();
    let mut expected: Vec<String> = [0x11, 0x12, 0x31, 0xee].iter().map(|t| hex::encode(txid(*t))).collect();
    expected.sort();
    assert_eq!(requested, expected, "single-carrier 0x21 is skipped");
    assert_eq!(accepting(&db.pool, 0x21).await.1, None);
    assert_eq!(accepting(&db.pool, 0x31).await.1, Some(561_000_049));
}
