mod parser;
mod router;
mod validator;

pub use parser::{
    extract_column_references, extract_equality_filters, extract_group_by_columns,
    extract_order_by_columns, extract_raw_column_predicates, AbiParam, AbiType, EventSignature,
};
pub use router::QueryEngine;
pub use validator::{
    engine_limit_cap, validate_query, ALLOWED_FUNCTIONS, HARD_LIMIT_CLICKHOUSE, HARD_LIMIT_MAX,
};

use regex_lite::Regex;
use std::sync::LazyLock;

/// Regex to match hex literals: '0x' followed by 40+ hex characters (addresses, topics, hashes)
/// This avoids matching short '0x' prefixes used in concat() expressions
static HEX_LITERAL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"'0x([0-9a-fA-F]{40,})'").unwrap());

/// Convert '0x...' hex literals to '\x...' for PostgreSQL bytea comparison.
/// Only replaces hex values of 40+ chars (addresses, topics, hashes), not short '0x' prefixes.
pub fn convert_hex_literals_postgres(sql: &str) -> String {
    HEX_LITERAL_RE
        .replace_all(sql, r"'\x$1'")
        .into_owned()
}

/// Lowercase the hex body of '0x...' literals for ClickHouse.
///
/// ClickHouse stores address/topic/hash columns as lowercase, 0x-prefixed
/// strings. A checksummed (mixed-case) literal like '0xAbC…' therefore compares
/// unequal and silently returns zero rows — the dangerous kind of bug for
/// reconciliation, since it looks like a real "no results". Postgres avoids
/// this only because `convert_hex_literals_postgres` rewrites the literal to a
/// bytea, which compares by value. Here we keep the string type and 0x prefix
/// (CH columns are strings, not bytea) and just fold the hex to lowercase.
/// Matching `convert_hex_literals_postgres`, only 40+ hex-char literals are
/// touched, so short '0x' prefixes inside `concat()` are left alone.
pub fn normalize_hex_literals_clickhouse(sql: &str) -> String {
    HEX_LITERAL_RE
        .replace_all(sql, |caps: &regex_lite::Captures| {
            format!("'0x{}'", caps[1].to_lowercase())
        })
        .into_owned()
}

#[cfg(test)]
mod hex_literal_tests {
    use super::normalize_hex_literals_clickhouse as ch;

    #[test]
    fn folds_mixed_case_address_to_lowercase() {
        let got = ch("SELECT * FROM logs WHERE address='0x17EC7E1768C813E2A3A9B0F94A35605CA520C242'");
        assert_eq!(
            got,
            "SELECT * FROM logs WHERE address='0x17ec7e1768c813e2a3a9b0f94a35605ca520c242'"
        );
    }

    #[test]
    fn folds_64_char_padded_topic() {
        let got = ch("WHERE topic2='0x000000000000000000000000C281CB25715EA8E46C9B916F22AE7C0F55B014D7'");
        assert_eq!(
            got,
            "WHERE topic2='0x000000000000000000000000c281cb25715ea8e46c9b916f22ae7c0f55b014d7'"
        );
    }

    #[test]
    fn folds_each_literal_in_an_in_list() {
        let got = ch("address IN ('0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA','0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB')");
        assert_eq!(
            got,
            "address IN ('0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb')"
        );
    }

    #[test]
    fn leaves_short_0x_prefixes_untouched() {
        // concat()-style short prefixes are below the 40-char threshold.
        let sql = "SELECT concat('0x', hex(x)) FROM t WHERE y='0xDEAD'";
        assert_eq!(ch(sql), sql);
    }

    #[test]
    fn already_lowercase_is_unchanged() {
        let sql = "WHERE address='0x17ec7e1768c813e2a3a9b0f94a35605ca520c242'";
        assert_eq!(ch(sql), sql);
    }
}
