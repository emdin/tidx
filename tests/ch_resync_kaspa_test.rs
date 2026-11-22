//! deploy/prod/ch-resync-kaspa.sh against scratch Postgres + ClickHouse databases.
//!
//! Runs only when RESYNC_TEST_PSQL and RESYNC_TEST_CH are set to the script's
//! PSQL / CH commands, and only on databases named *_test, because it drops and
//! recreates the Kaspa tables there. For example:
//!
//! RESYNC_TEST_PSQL="docker exec -i tidx-test-pg psql -U tidx -d tidx_resync_test" \
//! RESYNC_TEST_CH="docker exec -i tidx-test-ch clickhouse-client -d tidx_resync_test" \
//! cargo test --test ch_resync_kaspa_test

use serial_test::serial;
use std::process::{Command, Stdio};

struct Dbs {
    psql: String,
    ch: String,
}

fn run(cmd: &str, extra: &[&str]) -> String {
    let mut parts = cmd.split_whitespace();
    let out = Command::new(parts.next().unwrap())
        .args(parts)
        .args(extra)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{cmd} {extra:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

impl Dbs {
    fn from_env() -> Option<Self> {
        let (Ok(psql), Ok(ch)) = (
            std::env::var("RESYNC_TEST_PSQL"),
            std::env::var("RESYNC_TEST_CH"),
        ) else {
            println!("RESYNC_TEST_PSQL / RESYNC_TEST_CH not set, skipping");
            return None;
        };
        let dbs = Self { psql, ch };
        let (pg_db, ch_db) = (
            dbs.pg("SELECT current_database()"),
            dbs.ch("SELECT currentDatabase()"),
        );
        assert!(
            pg_db.ends_with("_test") && ch_db.ends_with("_test"),
            "refusing to drop tables in {pg_db} / {ch_db}: not *_test databases"
        );
        Some(dbs)
    }

    fn pg(&self, sql: &str) -> String {
        run(&self.psql, &["-v", "ON_ERROR_STOP=1", "-AtX", "-c", sql])
    }

    fn ch(&self, sql: &str) -> String {
        run(&self.ch, &["--query", sql])
    }

    fn script(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new("deploy/prod/ch-resync-kaspa.sh")
            .args(args)
            .env("PSQL", &self.psql)
            .env("CH", &self.ch)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    fn count(&self, table: &str) -> u64 {
        self.ch(&format!("SELECT count() FROM {table} FINAL"))
            .parse()
            .unwrap()
    }

    /// Postgres: 100 L2 txs from May, every 10th with 3 Kaspa carriers (120
    /// rows), and 20 entries. ClickHouse: empty tables from db/clickhouse, or,
    /// with `old_tables`, the pre-2026-11 tables as prod had them: keyed on
    /// l2_tx_hash, bare hex, one carrier per L2 tx and 5 entries missing.
    fn reset(&self, old_tables: bool) {
        self.pg(
            "DROP TABLE IF EXISTS kaspa_l2_submissions, kaspa_entries;
             CREATE TABLE kaspa_l2_submissions (l2_tx_hash BYTEA NOT NULL, kaspa_txid BYTEA PRIMARY KEY,
                                                created_at TIMESTAMPTZ NOT NULL DEFAULT now());
             CREATE TABLE kaspa_entries (kaspa_txid BYTEA PRIMARY KEY, recipient BYTEA NOT NULL,
                                         amount_sompi INT8 NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT now());
             INSERT INTO kaspa_l2_submissions
             SELECT decode(lpad(to_hex(i), 64, '0'), 'hex'), decode('97b1' || lpad(to_hex(i * 10 + c), 60, '0'), 'hex'),
                    '2026-05-01 12:00:00.123456+00'::timestamptz + i * interval '1 minute'
             FROM generate_series(1, 100) i, generate_series(0, 2) c WHERE c = 0 OR i % 10 = 0;
             INSERT INTO kaspa_entries
             SELECT decode('97b1' || lpad(to_hex(7000 + i), 60, '0'), 'hex'), decode(lpad(to_hex(i), 40, '0'), 'hex'),
                    i * 100, '2026-05-02 00:00:00+00'
             FROM generate_series(1, 20) i;",
        );
        for t in self
            .ch("SELECT name FROM system.tables WHERE database = currentDatabase()")
            .lines()
        {
            self.ch(&format!("DROP TABLE {t}"));
        }
        if !old_tables {
            for t in ["kaspa_l2_submissions", "kaspa_entries"] {
                self.ch(&std::fs::read_to_string(format!("db/clickhouse/{t}.sql")).unwrap());
            }
            return;
        }
        self.ch(
            "CREATE TABLE kaspa_l2_submissions (l2_tx_hash String, kaspa_txid String,
             created_at DateTime64(3, 'UTC')) ENGINE = ReplacingMergeTree() ORDER BY (l2_tx_hash)",
        );
        self.ch(
            "CREATE TABLE kaspa_entries (kaspa_txid String, recipient String, amount_sompi UInt64,
             created_at DateTime64(3, 'UTC')) ENGINE = ReplacingMergeTree() ORDER BY (recipient, kaspa_txid)",
        );
        self.ch(
            "INSERT INTO kaspa_l2_submissions
             SELECT leftPad(lower(hex(number + 1)), 64, '0'), '97b1' || leftPad(lower(hex((number + 1) * 10)), 60, '0'),
                    toDateTime64('2026-05-01 12:00:00.123', 3, 'UTC') + toIntervalMinute(number + 1)
             FROM numbers(100)",
        );
        self.ch(
            "INSERT INTO kaspa_entries
             SELECT '97b1' || leftPad(lower(hex(7000 + number + 1)), 60, '0'), leftPad(lower(hex(number + 1)), 40, '0'),
                    (number + 1) * 100, toDateTime64('2026-05-02 00:00:00', 3, 'UTC')
             FROM numbers(15)",
        );
    }
}

#[test]
#[serial(resync)]
fn copy_fills_the_tables_and_a_rerun_changes_nothing() {
    let Some(db) = Dbs::from_env() else { return };
    db.reset(false);

    let (code, out) = db.script(&[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(db.count("kaspa_l2_submissions"), 120);
    assert_eq!(db.count("kaspa_entries"), 20);
    assert_eq!(
        db.ch("SELECT kaspa_txid FROM kaspa_l2_submissions WHERE l2_tx_hash = '0x' || leftPad('a', 64, '0') ORDER BY 1"),
        ["0x97b1", "0x97b1", "0x97b1"]
            .iter()
            .zip([100, 101, 102])
            .map(|(p, n)| format!("{p}{:0>60x}", n))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let (code, out) = db.script(&[]);
    assert_eq!(code, 0, "{out}");
    // No duplicates left, even without FINAL.
    assert_eq!(db.ch("SELECT count() FROM kaspa_l2_submissions"), "120");
    assert_eq!(db.ch("SELECT count() FROM kaspa_entries"), "20");
}

#[test]
#[serial(resync)]
fn copy_refuses_old_tables_and_leaves_them_alone() {
    let Some(db) = Dbs::from_env() else { return };
    db.reset(true);

    let (code, out) = db.script(&[]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("--rebuild"), "{out}");
    assert_eq!(db.count("kaspa_l2_submissions"), 100);
    assert_eq!(
        db.ch("SELECT count() FROM kaspa_l2_submissions WHERE startsWith(l2_tx_hash, '0x')"),
        "0"
    );
}

#[test]
#[serial(resync)]
fn rebuild_restores_every_row_and_keeps_the_old_tables() {
    let Some(db) = Dbs::from_env() else { return };
    db.reset(true);
    // Promoted just now: copied by the fill and again by the catch-up.
    db.pg(
        "INSERT INTO kaspa_l2_submissions
         SELECT decode(lpad(to_hex(900 + i), 64, '0'), 'hex'), decode('97b1' || lpad(to_hex(9000 + i), 60, '0'), 'hex'), now()
         FROM generate_series(1, 3) i",
    );

    let (code, out) = db.script(&["--rebuild"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(db.ch("SELECT count() FROM kaspa_l2_submissions"), "123");
    assert_eq!(db.count("kaspa_entries"), 20);
    assert_eq!(
        db.ch("SELECT sorting_key FROM system.tables WHERE database = currentDatabase() AND name = 'kaspa_l2_submissions'"),
        "l2_tx_hash, kaspa_txid"
    );
    assert_eq!(
        db.ch(
            "SELECT replaceRegexpOne(name, '\\\\d{8}T\\\\d{6}Z$', 'TS'), total_rows FROM system.tables
             WHERE database = currentDatabase() AND name LIKE '%_before_%' ORDER BY name FORMAT TSV"
        ),
        "kaspa_entries_before_TS\t15\nkaspa_l2_submissions_before_TS\t100"
    );

    let (code, out) = db.script(&[]);
    assert_eq!(code, 0, "after the rebuild a plain copy is accepted: {out}");
}

#[test]
#[serial(resync)]
fn check_waits_for_rows_the_engine_is_still_mirroring_but_flags_older_gaps() {
    let Some(db) = Dbs::from_env() else { return };
    db.reset(false);
    assert_eq!(db.script(&[]).0, 0);

    db.pg(
        "INSERT INTO kaspa_l2_submissions VALUES (decode(repeat('ab', 32), 'hex'), decode(repeat('cd', 32), 'hex'), now())",
    );
    let (code, out) = db.script(&["--check"]);
    assert_eq!(
        code, 0,
        "a row promoted seconds ago may not be mirrored yet: {out}"
    );

    db.pg(
        "INSERT INTO kaspa_l2_submissions
         VALUES (decode(repeat('ef', 32), 'hex'), decode(repeat('12', 32), 'hex'), now() - interval '1 hour')",
    );
    let (code, out) = db.script(&["--check"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("postgres 121, clickhouse 120"), "{out}");
}

#[test]
#[serial(resync)]
fn hex_without_0x_in_any_column_fails_the_check() {
    let Some(db) = Dbs::from_env() else { return };
    db.reset(false);
    assert_eq!(db.script(&[]).0, 0);

    // Same row on both sides, but ClickHouse has the recipient without 0x.
    db.pg(
        "INSERT INTO kaspa_entries
         VALUES (decode(repeat('97', 32), 'hex'), decode(repeat('ee', 20), 'hex'), 1, '2026-05-03 00:00:00+00')",
    );
    db.ch(&format!(
        "INSERT INTO kaspa_entries VALUES ('0x{}', '{}', 1, '2026-05-03 00:00:00.000')",
        "97".repeat(32),
        "ee".repeat(20)
    ));
    let (code, out) = db.script(&["--check"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("hex without 0x"), "{out}");
}
