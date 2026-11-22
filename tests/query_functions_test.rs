//! Which functions public `/query` SQL may call.
//!
//! A tidx user's `startsWith(hash, '0x00') OR startsWith(...)` on ClickHouse got
//! "Function 'startswith' is not allowed" (2026-11-22): the allowlist had no
//! ClickHouse string or date functions at all, so some examples `/tables`
//! itself publishes could not run. The allowlist is security-critical (the
//! 2026-11-06 incident went through `/query`), so additions are pure functions
//! only, and the dangerous neighbours are pinned as blocked.

use tidx::api::tables_metadata;
use tidx::clickhouse::ClickHouseEngine;
use tidx::query::{HARD_LIMIT_CLICKHOUSE, validate_query};
use tidx::service::prepare_public_sql_postgres;

/// Each example goes through the same preparation `/query` gives it (lexing
/// guard, `?signature=` wrapping, allowlist) on every engine its table lists.
#[test]
fn every_example_published_in_tables_passes_the_query_checker() {
    let mut failing = Vec::new();
    for t in tables_metadata() {
        for e in &t.examples {
            let sigs: Vec<&str> = e.signature.into_iter().collect();
            for engine in &t.engines {
                let checked = match *engine {
                    "clickhouse" => {
                        ClickHouseEngine::prepare_public_sql(e.sql, &sigs, HARD_LIMIT_CLICKHOUSE)
                            .map(|_| ())
                    }
                    _ => prepare_public_sql_postgres(e.sql, &sigs).map(|_| ()),
                };
                if let Err(err) = checked {
                    failing.push(format!("{} / {} on {engine}: {err}", t.name, e.description));
                }
            }
        }
    }
    assert!(
        failing.is_empty(),
        "published examples that /query rejects: {failing:#?}"
    );
}

#[test]
fn clickhouse_prefix_and_suffix_matching_is_allowed() {
    for sql in [
        "SELECT count() FROM txs WHERE startsWith(hash, '0x00') OR startsWith(hash, '0x01')",
        "SELECT count() FROM txs WHERE endsWith(\"to\", 'dead')",
    ] {
        validate_query(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
}

#[test]
fn clickhouse_time_bucketing_is_allowed() {
    for f in [
        "toStartOfDay",
        "toStartOfHour",
        "toStartOfWeek",
        "toStartOfMonth",
        "toDate",
    ] {
        let sql = format!("SELECT {f}(block_timestamp) AS t, count() FROM blocks GROUP BY t");
        validate_query(&sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
}

#[test]
fn file_network_session_and_sleep_functions_stay_blocked() {
    for f in [
        "file('/etc/passwd')",
        "url('http://169.254.169.254/')",
        "remote('h:9000', 'db', 't')",
        "s3('https://x/y')",
        "input('x String')",
        "pg_read_file('/etc/passwd')",
        "pg_ls_dir('/')",
        "lo_export(1, '/tmp/x')",
        "set_config('role', 'tidx', false)",
        "current_setting('role')",
        "currentUser()",
        "getSetting('readonly')",
        "dictGet('d', 'a', 1)",
        "pg_sleep(1)",
        "sleep(1)",
    ] {
        let sql = format!("SELECT {f} FROM blocks");
        assert!(validate_query(&sql).is_err(), "{sql} must stay blocked");
    }
}
