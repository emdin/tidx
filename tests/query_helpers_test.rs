//! DB-backed tests for the SQL helper functions exposed through `/query`
//! (db/functions.sql). These cover the gaps a consuming dev reported:
//!   - `abi_uint(data, i)` / `format_uint(data, i)` word-indexed access for
//!     multi-word event `data` (e.g. a 64-byte AttesterRegistered payload),
//!   - `topic_addr('0xAbC…')` producing the 32-byte left-padded lowercase
//!     topic form from a checksummed address so topic filters stop silently
//!     returning zero rows.

mod common;

use common::testdb::TestDb;
use serial_test::serial;

/// Two ABI words: word 0 = 1, word 1 = 2^80 (25 digits, safely inside the
/// JSON layer's rust_decimal ceiling so we compare exact values).
fn two_word_data() -> Vec<u8> {
    let mut v = vec![0u8; 64];
    v[31] = 1; // word 0 = 1
    // word 1 = 2^80: byte at offset 32 + (31 - 10) = 53 set to 1
    v[53] = 1;
    v
}

#[tokio::test]
#[serial(db)]
async fn abi_uint_word_index_reads_the_right_32_byte_word() {
    let db = TestDb::empty().await;
    let conn = db.pool.get().await.unwrap();
    let data = two_word_data();

    let row = conn
        .query_one(
            "SELECT abi_uint($1, 0) AS w0, abi_uint($1, 1) AS w1",
            &[&data],
        )
        .await
        .expect("abi_uint(bytea, int) must exist and evaluate");

    let w0: rust_decimal::Decimal = row.get("w0");
    let w1: rust_decimal::Decimal = row.get("w1");
    assert_eq!(w0.to_string(), "1", "word 0 should decode to 1");
    assert_eq!(
        w1.to_string(),
        (rust_decimal::Decimal::from(2u64.pow(40)) * rust_decimal::Decimal::from(2u64.pow(40))).to_string(),
        "word 1 should decode to 2^80"
    );
}

#[tokio::test]
#[serial(db)]
async fn abi_uint_no_index_reads_whole_buffer_as_one_int() {
    // The 1-arg form reads the *entire* buffer as a single big-endian integer.
    // On 64-byte data that is (word0 << 256) | word1, which for our fixture is
    // (1 << 256) + 2^80 — proving the 1-arg form is wrong for multi-word data
    // and the indexed form is the fix.
    let db = TestDb::empty().await;
    let conn = db.pool.get().await.unwrap();
    let data = two_word_data();

    let row = conn
        .query_one("SELECT abi_uint($1)::TEXT AS whole", &[&data])
        .await
        .unwrap();
    let whole: String = row.get("whole");
    // 2^256 + 2^80 (word0=1 occupies the high 256 bits, word1=2^80 the low)
    let expected = "115792089237316195423570985008687907853269984665640565248383403622542304346112";
    assert_eq!(whole, expected);
}

#[tokio::test]
#[serial(db)]
async fn format_uint_word_index_returns_text_and_survives_large_values() {
    // format_uint returns TEXT, so it is not subject to the JSON layer's
    // rust_decimal (2^96) ceiling that nulls abi_uint. A full 32-byte 0xff
    // word (2^256-1, 78 digits) must round-trip as a string.
    let db = TestDb::empty().await;
    let conn = db.pool.get().await.unwrap();
    let mut data = vec![0u8; 64];
    for b in data.iter_mut().skip(32) {
        *b = 0xff; // word 1 = 2^256 - 1
    }

    let row = conn
        .query_one("SELECT format_uint($1, 1) AS w1", &[&data])
        .await
        .expect("format_uint(bytea, int) must exist");
    let w1: String = row.get("w1");
    assert_eq!(
        w1,
        "115792089237316195423570985008687907853269984665640564039457584007913129639935"
    );
}

#[tokio::test]
#[serial(db)]
async fn topic_addr_pads_and_lowercases_a_checksummed_address() {
    let db = TestDb::empty().await;
    let conn = db.pool.get().await.unwrap();

    // Mixed-case (EIP-55 checksummed) input must produce the same 32-byte
    // topic as the canonical lowercase padded form.
    let row = conn
        .query_one(
            "SELECT topic_addr('0xC281cb25715EA8e46c9B916F22aE7c0F55b014d7') AS t",
            &[],
        )
        .await
        .expect("topic_addr(text) must exist");
    let t: Vec<u8> = row.get("t");

    let mut expected = vec![0u8; 32];
    let addr = hex::decode("c281cb25715ea8e46c9b916f22ae7c0f55b014d7").unwrap();
    expected[12..].copy_from_slice(&addr);
    assert_eq!(t, expected, "topic_addr must left-pad to 32 bytes, lowercased");
}

#[tokio::test]
#[serial(db)]
async fn topic_addr_accepts_input_without_0x_prefix() {
    let db = TestDb::empty().await;
    let conn = db.pool.get().await.unwrap();
    let row = conn
        .query_one(
            "SELECT topic_addr('C281cb25715EA8e46c9B916F22aE7c0F55b014d7') = \
                    topic_addr('0xc281cb25715ea8e46c9b916f22ae7c0f55b014d7') AS eq",
            &[],
        )
        .await
        .unwrap();
    let eq: bool = row.get("eq");
    assert!(eq, "topic_addr must accept input with or without the 0x prefix");
}
