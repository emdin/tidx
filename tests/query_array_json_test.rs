//! `/query` JSON for Postgres array columns. `kaspa_*.l1_senders` (text[])
//! and `l1_sender_amounts_sompi` (int8[]) came back as `null` on every row
//! because the serializer had no array arms.
//!
//! Requires `DATABASE_URL` → `tidx-test-pg`.

mod common;

use common::testdb::TestDb;
use serde_json::json;
use serial_test::serial;
use tidx::service::format_column_json;

#[tokio::test]
#[serial(db)]
async fn array_columns_serialize_as_json_arrays() {
    let db = TestDb::empty().await;
    let conn = db.pool.get().await.unwrap();
    let row = conn
        .query_one(
            "SELECT ARRAY['kaspa:qa','kaspa:qb']::text[]   AS senders,
                    ARRAY[219100, NULL]::int8[]            AS amounts,
                    ARRAY[1, 2]::int4[]                    AS i4,
                    ARRAY['\\x97b1'::bytea]                AS txids,
                    ARRAY[true, false]                     AS flags,
                    '{}'::text[]                           AS empty,
                    NULL::text[]                           AS missing",
            &[],
        )
        .await
        .unwrap();

    assert_eq!(format_column_json(&row, 0), json!(["kaspa:qa", "kaspa:qb"]));
    assert_eq!(format_column_json(&row, 1), json!([219100, null]));
    assert_eq!(format_column_json(&row, 2), json!([1, 2]));
    assert_eq!(format_column_json(&row, 3), json!(["0x97b1"]));
    assert_eq!(format_column_json(&row, 4), json!([true, false]));
    assert_eq!(format_column_json(&row, 5), json!([]));
    assert_eq!(format_column_json(&row, 6), json!(null));
}
