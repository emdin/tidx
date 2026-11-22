//! Every value /query returns from Postgres is the value, never a silent null.
//!
//! 2026-11-22, prod: the renderer had no arm for jsonb, json, date, time,
//! timestamp (without time zone) or interval, and NUMERIC went through
//! rust_decimal, which cannot hold 2^96 or more. All of those came back as
//! null: `SELECT calls FROM txs` (jsonb), `date_trunc('day', t)::date`,
//! `now() - max(timestamp)`, a `sum()` of wei amounts. A type /query still
//! cannot render is now an error that names the column, not a null.

mod common;

use common::testdb::TestDb;
use serde_json::{Value, json};
use serial_test::serial;
use tidx::service::{QueryOptions, execute_query_postgres, format_column_json};

/// Render each value and ask Postgres for its own text form of the same value.
async fn rendered_vs_text(db: &TestDb, ty: &str, values: &[&str]) -> Vec<(String, Value, String)> {
    let conn = db.pool.get().await.unwrap();
    conn.batch_execute("SET IntervalStyle = 'postgres'")
        .await
        .unwrap();
    let mut out = Vec::new();
    for v in values {
        let row = conn
            .query_one(
                &format!("SELECT $1::text::{ty}, $1::text::{ty}::text"),
                &[v],
            )
            .await
            .unwrap();
        out.push((v.to_string(), format_column_json(&row, 0), row.get(1)));
    }
    out
}

#[tokio::test]
#[serial(db)]
async fn numeric_is_exact_at_any_size() {
    let db = TestDb::empty().await;
    let values = [
        "0",
        "1",
        "-1",
        "0.0001",
        "-0.0001",
        "1.50",
        "100.0",
        "9999.9999",
        "10000",
        "123456789.123456789",
        "0.00000000000000000001",
        "12345678901234567890123456789012345678",
        // 2^256 - 1 and -2^255: what abi_uint() and sums of wei produce.
        "115792089237316195423570985008687907853269984665640564039457584007913129639935",
        "-57896044618658097711785492504343953926634992332820282019728792003956564819968",
        "1e100",
        "NaN",
        "Infinity",
        "-Infinity",
    ];
    for (v, rendered, text) in rendered_vs_text(&db, "numeric", &values).await {
        assert_eq!(rendered, Value::String(text), "numeric {v}");
    }
}

#[tokio::test]
#[serial(db)]
async fn interval_reads_as_postgres_writes_it() {
    let db = TestDb::empty().await;
    let values = [
        "0",
        "1 day",
        "2 days",
        "-1 day",
        "1 mon",
        "13 mons",
        "-13 mons",
        "1 year",
        "1 year 2 mons 3 days 04:05:06.789",
        "-1 year -2 mons +3 days",
        "1 day -04:05:06",
        "-04:05:06",
        "30 hours",
        "00:00:00.000001",
        "-00:00:00.5",
        "1 day 00:00:01",
        "-1 year +1 day",
        "100 years 00:00:00.25",
    ];
    for (v, rendered, text) in rendered_vs_text(&db, "interval", &values).await {
        assert_eq!(rendered, Value::String(text), "interval {v}");
    }
}

#[tokio::test]
#[serial(db)]
async fn json_dates_times_and_arrays_render() {
    let db = TestDb::empty().await;
    let conn = db.pool.get().await.unwrap();
    let row = conn
        .query_one(
            "SELECT '{\"a\": [1, 2]}'::jsonb, '[1, \"x\"]'::json, date '2026-11-22',
                    time '17:05:06.5', timestamp '2026-11-22 17:05:06.123456',
                    ARRAY[1.5, 2]::float8[], ARRAY[1.5, NULL, 2e30]::numeric[],
                    ARRAY[timestamptz '2026-11-22 00:00+00'],
                    NULL::jsonb, NULL::date, NULL::interval, NULL::numeric",
            &[],
        )
        .await
        .unwrap();
    let got: Vec<Value> = (0..row.len())
        .map(|i| format_column_json(&row, i))
        .collect();
    assert_eq!(
        got,
        [
            json!({"a": [1, 2]}),
            json!([1, "x"]),
            json!("2026-11-22"),
            json!("17:05:06.500"),
            json!("2026-11-22T17:05:06.123456"),
            json!([1.5, 2.0]),
            json!(["1.5", null, "2000000000000000000000000000000"]),
            json!(["2026-11-22T00:00:00+00:00"]),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
        ]
    );
}

#[tokio::test]
#[serial(db)]
async fn public_query_returns_jsonb_and_names_a_column_it_cannot_render() {
    let db = TestDb::empty().await;
    {
        let conn = db.pool.get().await.unwrap();
        conn.batch_execute(
            "DELETE FROM kaspa_provenance_gaps;
             INSERT INTO kaspa_provenance_gaps (reason, details)
             VALUES ('test', '{\"blocks\": 3}')",
        )
        .await
        .unwrap();
    }
    let options = QueryOptions {
        timeout_ms: 5_000,
        limit: 10,
    };

    let ok = execute_query_postgres(
        &db.pool,
        "SELECT details, started_at::date AS day, now() - started_at < interval '1 hour' AS fresh FROM kaspa_provenance_gaps",
        &[],
        &options,
    )
    .await
    .unwrap();
    assert_eq!(ok.rows[0][0], json!({"blocks": 3}));
    assert!(ok.rows[0][1].is_string(), "{:?}", ok.rows[0][1]);
    assert_eq!(ok.rows[0][2], json!(true));

    let err = execute_query_postgres(
        &db.pool,
        "SELECT reason, '00000000-0000-0000-0000-000000000000'::uuid AS u FROM kaspa_provenance_gaps",
        &[],
        &options,
    )
    .await
    .expect_err("a type /query cannot render is an error, not a null");
    let msg = err.to_string();
    assert!(
        msg.contains("\"u\"") && msg.contains("uuid") && msg.contains("::text"),
        "{msg}"
    );
}

/// Seeded random values over wide ranges: mixed signs, scales up to 30, up to
/// ~250 digits, intervals mixing every field with both signs.
#[tokio::test]
#[serial(db)]
async fn numeric_and_interval_match_postgres_on_generated_values() {
    let db = TestDb::empty().await;
    let conn = db.pool.get().await.unwrap();
    conn.batch_execute("SET IntervalStyle = 'postgres'; SELECT setseed(0.42)")
        .await
        .unwrap();
    let rows = conn
        .query(
            "SELECT v, v::text FROM (
               SELECT round(((random() - 0.5)::numeric * 10::numeric ^ (trunc(random() * 60) - 30)::numeric) ^ (1 + trunc(random() * 4))::numeric,
                            trunc(random() * 30)::int) AS v
               FROM generate_series(1, 2000)) t",
            &[],
        )
        .await
        .unwrap();
    for row in &rows {
        let text: String = row.get(1);
        assert_eq!(
            format_column_json(row, 0),
            Value::String(text.clone()),
            "numeric {text}"
        );
    }
    let rows = conn
        .query(
            "SELECT v, v::text FROM (
               SELECT make_interval(years => (random() * 20 - 10)::int, months => (random() * 30 - 15)::int,
                                    days => (random() * 60 - 30)::int, hours => (random() * 100 - 50)::int,
                                    mins => (random() * 120 - 60)::int,
                                    secs => round((random() * 120 - 60)::numeric, (random() * 6)::int)::float8)
                      * (random() < 0.2)::int AS v
               FROM generate_series(1, 1000)) t",
            &[],
        )
        .await
        .unwrap();
    for row in &rows {
        let text: String = row.get(1);
        assert_eq!(
            format_column_json(row, 0),
            Value::String(text.clone()),
            "interval {text}"
        );
    }
}
