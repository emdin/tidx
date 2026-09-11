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

use sqlparser::ast::{BinaryOperator, Expr, Value, visit_expressions_mut};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;
use std::ops::ControlFlow;

/// Enforce a row-count ceiling on a query.
///
/// - No top-level LIMIT → append `LIMIT append_if_missing` (the caller's
///   requested/default page size, already ≤ `cap`).
/// - `LIMIT ALL` / no numeric limit → set to `cap`.
/// - Explicit numeric `LIMIT n` with `n > cap` → clamp to `cap`. Previously an
///   explicit LIMIT bypassed the published cap (ClickHouse had no enforcement
///   at all, so `LIMIT 50001` returned 50001 rows against a 50000 cap).
/// - `LIMIT n` with `n ≤ cap` → left as-is (OFFSET / `BY` preserved).
///
/// Callers pass SQL that `validate_query` has already parsed; an unexpected
/// parse failure returns the input unchanged rather than risk corrupting it.
pub fn enforce_limit(sql: &str, append_if_missing: i64, cap: i64) -> String {
    use sqlparser::ast::{Expr, LimitClause, Statement, Value};
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;

    let num = |v: i64| Expr::Value(Value::Number(v.to_string(), false).with_empty_span());
    let over_cap = |e: &Expr| -> bool {
        matches!(e, Expr::Value(vws) if matches!(&vws.value,
            Value::Number(n, _) if n.parse::<i64>().map(|v| v > cap).unwrap_or(false)))
    };

    let mut stmts = match Parser::parse_sql(&GenericDialect {}, sql) {
        Ok(s) if s.len() == 1 => s,
        _ => return sql.to_string(),
    };

    if let Statement::Query(q) = &mut stmts[0] {
        match &mut q.limit_clause {
            None => {
                q.limit_clause = Some(LimitClause::LimitOffset {
                    limit: Some(num(append_if_missing)),
                    offset: None,
                    limit_by: Vec::new(),
                });
            }
            Some(LimitClause::LimitOffset { limit, .. }) => match limit {
                Some(e) if over_cap(e) => *limit = Some(num(cap)),
                None => *limit = Some(num(cap)), // LIMIT ALL / offset-only
                _ => {}
            },
            Some(LimitClause::OffsetCommaLimit { limit, .. }) => {
                if over_cap(limit) {
                    *limit = num(cap);
                }
            }
        }
    }

    stmts[0].to_string()
}

/// The hex body of `s` if it is an address/topic/hash literal: `0x` plus 40+
/// hex chars. Shorter `'0x'` fragments (e.g. inside `concat()`) don't qualify.
fn hex_body(s: &str) -> Option<&str> {
    let body = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    (body.len() >= 40 && body.bytes().all(|b| b.is_ascii_hexdigit())).then_some(body)
}

/// A bare column reference, looking through parentheses: `addr`, `t.addr`, `(addr)`.
fn is_column(e: &Expr) -> bool {
    match e {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => true,
        Expr::Nested(inner) => is_column(inner),
        _ => false,
    }
}

/// Rewrite `e` in place if it is a qualifying `'0x…'` string literal.
fn rewrite_literal(e: &mut Expr, transform: &dyn Fn(&str) -> String) {
    let Expr::Value(v) = e else { return };
    let Value::SingleQuotedString(s) = &v.value else { return };
    if let Some(body) = hex_body(s) {
        *e = Expr::Value(Value::SingleQuotedString(transform(body)).with_empty_span());
    }
}

/// Rewrite `'0x…'` hex literals, but ONLY where they are a direct operand of a
/// comparison (`= <> < > <= >=`), an `IN (…)` list, or a `BETWEEN`, against a
/// bare column reference. `transform` receives the hex body and returns the
/// replacement string contents.
///
/// This is deliberately context-aware. The previous implementation was a raw
/// regex over the SQL text, which also rewrote literals passed as *function
/// arguments* — so `topic_addr('0x…')` became `topic_addr('\x…')` (→ "invalid
/// hexadecimal digit") and `replace('0x…','0x','')` was mangled. A function
/// argument is never a direct comparison operand, so walking the AST and only
/// touching comparison operands fixes both while preserving the convenience
/// (`WHERE addr = '0x…'`, `IN ('0x…', …)`). Every literal the `?signature=`
/// CTE generator emits is also of the `col = '…'` form, so those still convert.
///
/// Callers always pass SQL that `validate_query` has already parsed as a single
/// SELECT, so a parse failure here is not expected; if it happens the input is
/// returned unchanged rather than risk corrupting it.
fn rewrite_hex_literals(sql: &str, transform: impl Fn(&str) -> String) -> String {
    use BinaryOperator::{Eq, Gt, GtEq, Lt, LtEq, NotEq};

    let mut statements = match Parser::parse_sql(&GenericDialect {}, sql) {
        Ok(s) if s.len() == 1 => s,
        _ => return sql.to_string(),
    };

    // `col <op> lit` or `lit <op> col`: rewrite whichever side faces a column.
    let rewrite_facing = |a: &mut Expr, b: &mut Expr| {
        if is_column(a) {
            rewrite_literal(b, &transform);
        }
    };

    let _: ControlFlow<()> = visit_expressions_mut(&mut statements, |e| {
        match e {
            Expr::BinaryOp { left, op, right }
                if matches!(op, Eq | NotEq | Lt | Gt | LtEq | GtEq) =>
            {
                rewrite_facing(left, right);
                rewrite_facing(right, left);
            }
            Expr::InList { expr, list, .. } if is_column(expr) => {
                list.iter_mut().for_each(|item| rewrite_literal(item, &transform));
            }
            Expr::Between { expr, low, high, .. } if is_column(expr) => {
                rewrite_literal(low, &transform);
                rewrite_literal(high, &transform);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    });

    statements[0].to_string()
}

/// Convert `'0x…'` hex literals to `'\x…'` so Postgres reads them as bytea.
/// See [`rewrite_hex_literals`] for why this is AST-scoped, not textual.
pub fn convert_hex_literals_postgres(sql: &str) -> String {
    rewrite_hex_literals(sql, |body| format!("\\x{body}"))
}

/// Lowercase the hex body of `'0x…'` literals for ClickHouse.
///
/// ClickHouse stores address/topic/hash columns as lowercase 0x-prefixed
/// strings and compares them case-sensitively, so a checksummed literal
/// silently returns zero rows — the dangerous kind of bug for reconciliation.
/// Postgres avoids this only because it rewrites the literal to bytea (compared
/// by value). Here we keep the string type and 0x prefix and fold to lowercase,
/// scoped to comparison operands so a function argument is not clobbered.
pub fn normalize_hex_literals_clickhouse(sql: &str) -> String {
    rewrite_hex_literals(sql, |body| format!("0x{}", body.to_ascii_lowercase()))
}

#[cfg(test)]
mod enforce_limit_tests {
    use super::enforce_limit;

    // append_if_missing = 100, cap = 1000 for these unless noted.
    fn el(sql: &str) -> String {
        enforce_limit(sql, 100, 1000)
    }

    #[test]
    fn appends_limit_when_absent() {
        assert_eq!(el("SELECT n FROM blocks"), "SELECT n FROM blocks LIMIT 100");
    }

    #[test]
    fn clamps_explicit_limit_over_cap() {
        // The reported bug: LIMIT above the cap must be reduced to the cap.
        assert_eq!(el("SELECT n FROM blocks LIMIT 50001"), "SELECT n FROM blocks LIMIT 1000");
    }

    #[test]
    fn leaves_explicit_limit_under_cap() {
        assert_eq!(el("SELECT n FROM blocks LIMIT 5"), "SELECT n FROM blocks LIMIT 5");
    }

    #[test]
    fn limit_all_becomes_bounded() {
        // sqlparser normalizes `LIMIT ALL` to no limit clause, so it takes the
        // append path and is bounded (not left unbounded). Exact value aside,
        // the guarantee is: no unbounded `ALL` survives.
        let out = el("SELECT n FROM blocks LIMIT ALL");
        assert!(!out.contains("ALL"), "LIMIT ALL must not survive unbounded; got: {out}");
        assert!(out.contains("LIMIT 100"), "bounded to the page size; got: {out}");
    }

    #[test]
    fn preserves_offset_when_appending() {
        // OFFSET-only (no row limit) is treated as unbounded -> capped.
        let out = el("SELECT n FROM blocks OFFSET 10");
        assert!(out.contains("LIMIT 1000"), "offset-only must gain a cap; got: {out}");
        assert!(out.contains("OFFSET 10"), "existing OFFSET preserved; got: {out}");
    }

    #[test]
    fn preserves_under_cap_limit_with_offset() {
        let out = el("SELECT n FROM blocks LIMIT 5 OFFSET 10");
        assert!(out.contains("LIMIT 5") && out.contains("OFFSET 10"), "got: {out}");
    }

    #[test]
    fn unparseable_returns_input_unchanged() {
        let sql = "not sql (((";
        assert_eq!(enforce_limit(sql, 100, 1000), sql);
    }

    #[test]
    fn appends_caller_page_size_not_cap() {
        // When absent, we append the caller's requested size (<= cap), not the cap.
        assert_eq!(enforce_limit("SELECT 1", 250, 50000), "SELECT 1 LIMIT 250");
    }
}

#[cfg(test)]
mod hex_literal_tests {
    use super::{convert_hex_literals_postgres as pg, normalize_hex_literals_clickhouse as ch};

    const ADDR: &str = "0x17EC7E1768C813E2A3A9B0F94A35605CA520C242";
    const ADDR_LC: &str = "17ec7e1768c813e2a3a9b0f94a35605ca520c242";

    // ---- Postgres: '0x…' -> '\x…' in comparison contexts only ----

    #[test]
    fn pg_converts_column_eq_literal() {
        let out = pg(&format!("SELECT n FROM logs WHERE address = '{ADDR}'"));
        assert!(out.contains(&format!("'\\x{ADDR_LC}'")) || out.contains(&format!("'\\x{}'", &ADDR[2..])),
            "column = '0x…' must become a bytea '\\x…' literal; got: {out}");
        assert!(!out.contains("'0x1"), "no 0x literal should remain in a comparison; got: {out}");
    }

    #[test]
    fn pg_converts_in_list() {
        // PG bytea hex input is case-insensitive, so the body case is preserved
        // (only ClickHouse mode lowercases). Just assert both became '\x…'.
        let out = pg("SELECT n FROM logs WHERE address IN ('0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA','0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB')");
        assert!(out.matches("'\\x").count() == 2, "both IN-list literals convert to '\\x…'; got: {out}");
        assert!(!out.contains("'0x"), "no 0x literal should remain; got: {out}");
    }

    #[test]
    fn pg_converts_literal_on_left_side() {
        let out = pg(&format!("SELECT n FROM logs WHERE '{ADDR}' = address"));
        assert!(out.contains("'\\x"), "literal-on-left must also convert; got: {out}");
    }

    /// The reported bug: a hex literal that is a FUNCTION ARGUMENT must be left
    /// untouched (topic_addr / replace / decode all take text, not bytea).
    #[test]
    fn pg_leaves_function_argument_untouched() {
        let out = pg(&format!("SELECT n FROM logs WHERE topic2 = topic_addr('{ADDR}')"));
        assert!(out.contains("topic_addr('0x") || out.to_lowercase().contains("topic_addr('0x"),
            "topic_addr's 0x argument must survive intact; got: {out}");
        assert!(!out.contains("\\x"), "no bytea escape should be introduced into a function arg; got: {out}");
    }

    #[test]
    fn pg_leaves_replace_argument_untouched() {
        let sql = "SELECT replace('0xC281cb25715EA8e46c9B916F22aE7c0F55b014d7','0x','') AS a";
        let out = pg(sql);
        assert!(out.contains("replace('0x"), "replace()'s 0x arg must not be mangled; got: {out}");
        assert!(!out.contains("\\x"), "got: {out}");
    }

    #[test]
    fn pg_leaves_select_item_literal_untouched() {
        // A bare projected literal is not a comparison operand.
        let out = pg(&format!("SELECT '{ADDR}' AS a"));
        assert!(!out.contains("\\x"), "projected literal should not convert; got: {out}");
    }

    #[test]
    fn pg_converts_parenthesised_column_and_between() {
        let out = pg(&format!("SELECT n FROM logs WHERE (address) = '{ADDR}' AND topic1 BETWEEN '{ADDR}' AND '{ADDR}'"));
        assert_eq!(out.matches("'\\x").count(), 3, "(col) = lit and BETWEEN bounds all convert; got: {out}");
    }

    #[test]
    fn pg_converts_not_equal_and_ordering_comparisons() {
        let out = pg(&format!("SELECT n FROM logs WHERE address <> '{ADDR}' AND topic1 > '{ADDR}'"));
        assert_eq!(out.matches("'\\x").count(), 2, "<> and > operands convert; got: {out}");
    }

    #[test]
    fn pg_leaves_short_0x_untouched() {
        let out = pg("SELECT concat('0x', encode(hash,'hex')) FROM blocks WHERE num = 5");
        assert!(out.contains("'0x'"), "short '0x' prefix must survive; got: {out}");
    }

    #[test]
    fn unparseable_returns_input_unchanged() {
        let sql = "this is not valid sql ((";
        assert_eq!(pg(sql), sql);
    }

    // ---- ClickHouse: lowercase the hex body in comparison contexts only ----

    #[test]
    fn ch_lowercases_column_comparison() {
        let out = ch(&format!("SELECT n FROM logs WHERE address = '{ADDR}'"));
        assert!(out.contains(&format!("'0x{ADDR_LC}'")), "must lowercase to CH's stored form; got: {out}");
    }

    #[test]
    fn ch_leaves_function_argument_case_intact() {
        // Folding a function arg's case could change its meaning; scope to comparisons.
        let out = ch(&format!("SELECT lower(replace('{ADDR}','0x','')) AS a WHERE address = '{ADDR}'"));
        assert!(out.contains(&format!("replace('{ADDR}'")), "func-arg case must be preserved; got: {out}");
        assert!(out.contains(&format!("address = '0x{ADDR_LC}'")), "the comparison operand still folds; got: {out}");
    }

    #[test]
    fn ch_unparseable_returns_input_unchanged() {
        let sql = ")))(((";
        assert_eq!(ch(sql), sql);
    }
}
