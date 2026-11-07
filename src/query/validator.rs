use std::collections::HashSet;
use std::ops::ControlFlow;

use anyhow::{Result, anyhow};
use sqlparser::ast::{
    ArrayElemTypeDef, DataType, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments,
    ObjectName, Query, SetExpr, Statement, TableFactor, TableWithJoins, Visit, Visitor,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

pub const ALLOWED_TABLES: &[&str] = &[
    "blocks",
    "txs",
    "logs",
    "receipts",
    "l2_withdrawals",
    "internal_txs",
    "trace_outcomes",
    "kaspa_provenance_meta",
    "kaspa_sync_state",
    "kaspa_provenance_gaps",
    "kaspa_pending_l2_submissions",
    "kaspa_pending_entries",
    "kaspa_l2_submissions",
    "kaspa_entries",
    "reorgs",
    "orphaned_blocks",
    "orphaned_txs",
    "orphaned_logs",
    "orphaned_receipts",
    "orphaned_internal_txs",
    "orphaned_l2_withdrawals",
];

const MAX_QUERY_LENGTH: usize = 65_536;
const MAX_SUBQUERY_DEPTH: usize = 4;

/// Maximum row count per page for the default (Postgres) engine. PG's planner
/// is sensitive to large LIMITs, especially when combined with non-indexed
/// ORDER BY, so we keep this conservative.
pub const HARD_LIMIT_MAX: i64 = 10_000;

/// Maximum row count per page when the caller explicitly opts into ClickHouse
/// via `?engine=clickhouse`. CH is built for analytical scans and handles
/// 50k-row result sets routinely, so the cap can be higher.
pub const HARD_LIMIT_CLICKHOUSE: i64 = 50_000;

/// Resolve the per-page row cap for a given `?engine=` parameter. Unknown,
/// missing, or postgres values fall back to the conservative cap so callers
/// who don't opt in to ClickHouse don't accidentally get 5x the result size.
pub fn engine_limit_cap(engine: Option<&str>) -> i64 {
    match engine {
        Some("clickhouse") => HARD_LIMIT_CLICKHOUSE,
        _ => HARD_LIMIT_MAX,
    }
}

/// Validates that a SQL query is safe to execute, with the Postgres row cap
/// (`HARD_LIMIT_MAX`) applied to the top-level `LIMIT`.
pub fn validate_query(sql: &str) -> Result<()> {
    validate_query_with_cap(sql, HARD_LIMIT_MAX)
}

/// Validates that a SQL query is safe to execute.
///
/// Uses a reject-by-default approach: only explicitly allowed tables,
/// functions, and expression types are permitted. Everything else is rejected.
///
/// `cap` bounds the TOP-LEVEL `LIMIT` and is engine policy (Postgres 10 000,
/// ClickHouse 50 000). Nested queries (CTEs, subqueries) are always held to
/// `HARD_LIMIT_MAX` regardless of engine: that check is a resource guard
/// against materialising huge intermediate sets, not the user-facing page cap,
/// and 10 000 is the right ceiling for it on both engines.
pub fn validate_query_with_cap(sql: &str, cap: i64) -> Result<()> {
    if sql.len() > MAX_QUERY_LENGTH {
        return Err(anyhow!(
            "Query too large ({} bytes, max {})",
            sql.len(),
            MAX_QUERY_LENGTH
        ));
    }

    let dialect = GenericDialect {};
    let statements =
        Parser::parse_sql(&dialect, sql).map_err(|e| anyhow!("SQL parse error: {e}"))?;

    if statements.is_empty() {
        return Err(anyhow!("Empty query"));
    }

    if statements.len() > 1 {
        return Err(anyhow!("Multiple statements not allowed"));
    }

    let stmt = &statements[0];

    match stmt {
        Statement::Query(query) => {
            let cte_names = extract_cte_names(query);
            validate_query_ast(query, &cte_names, 0)?;
            reject_hidden_calls(stmt)?;
            validate_limit_clause(query, cap)
        }
        _ => Err(anyhow!("Only SELECT queries are allowed")),
    }
}

/// The database a validated query will run on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqlEngine {
    Postgres,
    ClickHouse,
}

/// Reject raw SQL whose strings, identifiers or comments the target database
/// could tokenize differently from sqlparser's `GenericDialect`, which is what
/// validates it. If the two disagree on where a string ends, text the validator
/// saw as string content runs as SQL (2026-11-06 review: ClickHouse reads `\'`
/// inside '...' as an escaped quote; sqlparser does not).
///
/// Rejected everywhere: `$` (dollar-quoted strings / heredocs) and nested
/// comments. ClickHouse also: any backslash and `#` comments. Postgres also:
/// `E'...'` and `U&'...'` strings; plain '...' keeps backslashes literal there
/// (standard_conforming_strings is pinned on for public queries).
pub fn reject_ambiguous_lexing(sql: &str, engine: SqlEngine) -> Result<()> {
    let b = sql.as_bytes();
    let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let reject = |what: &str| Err(anyhow!("{what} is not allowed in /query SQL"));
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                if engine == SqlEngine::Postgres && q != b'`' {
                    let prefixed = |at: usize, set: &[u8]| {
                        i > at && set.contains(&b[i - 1 - at]) && (i < at + 2 || !is_ident(b[i - 2 - at]))
                    };
                    if q == b'\'' && prefixed(0, b"eE") {
                        return reject("An E'...' escape string");
                    }
                    // U&"..." identifiers too: sqlparser reads `U & "..."`, so a
                    // cast to U&"\0072egclass" passed the type check (#45 review).
                    if i >= 2 && b[i - 1] == b'&' && prefixed(1, b"uU") {
                        return reject("A U&'...' string or U&\"...\" identifier");
                    }
                }
                i += 1;
                loop {
                    match b.get(i) {
                        None => return Err(anyhow!("Unterminated quoted text")),
                        Some(&c) if c == q && b.get(i + 1) == Some(&q) => i += 2,
                        Some(&c) if c == q => break,
                        Some(b'\\') if engine == SqlEngine::ClickHouse => {
                            return reject("A backslash");
                        }
                        Some(_) => i += 1,
                    }
                }
                i += 1;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                loop {
                    match (b.get(i), b.get(i + 1)) {
                        (None, _) => return Err(anyhow!("Unterminated comment")),
                        (Some(b'*'), Some(b'/')) => break,
                        (Some(b'/'), Some(b'*')) => return reject("A nested comment"),
                        _ => i += 1,
                    }
                }
                i += 2;
            }
            b'$' => return reject("A '$' (dollar quoting)"),
            b'#' if engine == SqlEngine::ClickHouse => return reject("A '#' comment"),
            b'\\' if engine == SqlEngine::ClickHouse => return reject("A backslash"),
            _ => i += 1,
        }
    }
    Ok(())
}

/// Second pass over the whole statement that does not depend on the match arms
/// in `validate_expr`: every function call anywhere must be allowlisted and no
/// table function may appear. A missed arm (2026-11-06: the left side of
/// `= ANY(...)`) then cannot reopen the hole on its own.
fn reject_hidden_calls(stmt: &Statement) -> Result<()> {
    struct CteNames(HashSet<String>);

    impl Visitor for CteNames {
        type Break = ();

        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            self.0.extend(extract_cte_names(query));
            ControlFlow::Continue(())
        }
    }

    struct HiddenCalls<'a> {
        cte_names: &'a HashSet<String>,
    }

    impl Visitor for HiddenCalls<'_> {
        type Break = String;

        // Clauses the first pass never looks at (DISTINCT ON, QUALIFY, named
        // WINDOW, PREWHERE, ...) can hold subqueries: every table must still
        // be allowlisted.
        fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<String> {
            match validate_table_name(relation, self.cte_names) {
                Ok(()) => ControlFlow::Continue(()),
                Err(e) => ControlFlow::Break(e.to_string()),
            }
        }

        // SETTINGS would let a caller change ClickHouse settings under readonly=2.
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<String> {
            if query.settings.is_some()
                || query.format_clause.is_some()
                || query.for_clause.is_some()
                || !query.pipe_operators.is_empty()
            {
                return ControlFlow::Break(
                    "SETTINGS, FORMAT, FOR and pipe clauses are not allowed".to_string(),
                );
            }
            ControlFlow::Continue(())
        }

        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<String> {
            let cast_to = match expr {
                Expr::Function(func) => {
                    let name = func.name.to_string().to_lowercase();
                    if !is_allowed_function(&name) {
                        return ControlFlow::Break(format!("Function '{name}' is not allowed"));
                    }
                    None
                }
                Expr::Cast { data_type, .. } => Some(data_type),
                Expr::TypedString(ts) => Some(&ts.data_type),
                _ => None,
            };
            if cast_to.is_some_and(is_catalog_lookup_type) {
                return ControlFlow::Break(
                    "Casts to reg* catalog types are not allowed".to_string(),
                );
            }
            ControlFlow::Continue(())
        }

        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<String> {
            match factor {
                TableFactor::Table { args: Some(_), .. }
                | TableFactor::TableFunction { .. }
                | TableFactor::Function { .. } => {
                    ControlFlow::Break("Table functions are not allowed".to_string())
                }
                _ => ControlFlow::Continue(()),
            }
        }
    }

    let mut ctes = CteNames(HashSet::new());
    let _ = stmt.visit(&mut ctes);
    match stmt.visit(&mut HiddenCalls { cte_names: &ctes.0 }) {
        ControlFlow::Break(reason) => Err(anyhow!(reason)),
        ControlFlow::Continue(()) => Ok(()),
    }
}

/// PostgreSQL's reg* types (regclass, regprocedure, regtype, regrole, ...) look
/// a name up in the system catalogs while casting, so `'pg_authid'::regclass`
/// tells a caller whether any relation exists, whatever the table allowlist
/// says. No other built-in PostgreSQL or ClickHouse type name starts with "reg".
/// Only type names are checked (through arrays and wrappers), not field names
/// or literal arguments such as time zones and enum labels.
fn is_catalog_lookup_type(data_type: &DataType) -> bool {
    match data_type {
        DataType::Regclass => true,
        DataType::Custom(name, _) => name
            .0
            .last()
            .and_then(|part| part.as_ident())
            .is_some_and(|ident| ident.value.to_ascii_lowercase().starts_with("reg")),
        DataType::Array(
            ArrayElemTypeDef::AngleBracket(inner)
            | ArrayElemTypeDef::SquareBracket(inner, _)
            | ArrayElemTypeDef::Parenthesis(inner),
        )
        | DataType::Nullable(inner)
        | DataType::LowCardinality(inner) => is_catalog_lookup_type(inner),
        DataType::Map(key, value) => is_catalog_lookup_type(key) || is_catalog_lookup_type(value),
        DataType::Tuple(fields) | DataType::Struct(fields, _) => {
            fields.iter().any(|f| is_catalog_lookup_type(&f.field_type))
        }
        DataType::Nested(columns) => columns.iter().any(|c| is_catalog_lookup_type(&c.data_type)),
        _ => false,
    }
}

fn extract_cte_names(query: &Query) -> HashSet<String> {
    let mut names = HashSet::new();
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            names.insert(cte.alias.name.value.to_lowercase());
        }
    }
    names
}

fn validate_query_ast(query: &Query, cte_names: &HashSet<String>, depth: usize) -> Result<()> {
    if depth > MAX_SUBQUERY_DEPTH {
        return Err(anyhow!(
            "Subquery nesting too deep (max {} levels)",
            MAX_SUBQUERY_DEPTH
        ));
    }

    // Block recursive CTEs (can cause endless loops / resource exhaustion)
    if let Some(with) = &query.with {
        if with.recursive {
            return Err(anyhow!("Recursive CTEs are not allowed"));
        }
    }

    // Block FOR UPDATE / FOR SHARE locking clauses
    if !query.locks.is_empty() {
        return Err(anyhow!(
            "Locking clauses (FOR UPDATE/SHARE) are not allowed"
        ));
    }

    // Block FETCH clause (alternative to LIMIT, could bypass cap)
    if query.fetch.is_some() {
        return Err(anyhow!("FETCH clause is not allowed, use LIMIT instead"));
    }

    let mut all_cte_names = cte_names.clone();
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            all_cte_names.insert(cte.alias.name.value.to_lowercase());
        }
    }

    for cte in &query.with.as_ref().map_or(vec![], |w| w.cte_tables.clone()) {
        validate_query_ast(&cte.query, &all_cte_names, depth + 1)?;
    }

    validate_set_expr(&query.body, &all_cte_names, depth)?;

    // Validate ORDER BY expressions
    if let Some(order_by) = &query.order_by {
        match &order_by.kind {
            sqlparser::ast::OrderByKind::Expressions(exprs) => {
                for order_expr in exprs {
                    validate_expr(&order_expr.expr, &all_cte_names, depth)?;
                }
            }
            sqlparser::ast::OrderByKind::All(_) => {}
        }
    }

    // Nested queries get the fixed resource-guard cap; the top level is
    // checked by the caller with the engine's own cap.
    if depth > 0 {
        validate_limit_clause(query, HARD_LIMIT_MAX)?;
    }

    Ok(())
}

/// Validate LIMIT / OFFSET: numeric literals only, `LIMIT` at most `cap`.
fn validate_limit_clause(query: &Query, cap: i64) -> Result<()> {
    use sqlparser::ast::LimitClause;
    match &query.limit_clause {
        None => Ok(()),
        Some(LimitClause::LimitOffset { limit, offset, limit_by }) => {
            if let Some(limit) = limit {
                validate_limit_expr(limit, "LIMIT", cap)?;
            }
            if let Some(offset) = offset {
                validate_limit_expr(&offset.value, "OFFSET", cap)?;
            }
            if !limit_by.is_empty() {
                return Err(anyhow!("LIMIT BY is not allowed"));
            }
            Ok(())
        }
        Some(LimitClause::OffsetCommaLimit { offset, limit }) => {
            validate_limit_expr(offset, "OFFSET", cap)?;
            validate_limit_expr(limit, "LIMIT", cap)
        }
    }
}

fn validate_limit_expr(expr: &Expr, context: &str, cap: i64) -> Result<()> {
    match expr {
        Expr::Value(v) => {
            let val = &v.value;
            match val {
                sqlparser::ast::Value::Number(n, _) => {
                    if let Ok(num) = n.parse::<i64>() {
                        if num < 0 {
                            return Err(anyhow!("{context} must not be negative"));
                        }
                        if num > cap {
                            return Err(anyhow!("{context} value {num} exceeds maximum ({cap})"));
                        }
                        Ok(())
                    } else {
                        Err(anyhow!("{context} must be a valid integer"))
                    }
                }
                sqlparser::ast::Value::Null => Err(anyhow!("{context} NULL is not allowed")),
                _ => Err(anyhow!("{context} must be a numeric literal")),
            }
        }
        _ => Err(anyhow!("{context} must be a numeric literal")),
    }
}

fn validate_set_expr(set_expr: &SetExpr, cte_names: &HashSet<String>, depth: usize) -> Result<()> {
    match set_expr {
        SetExpr::Select(select) => {
            // Reject SELECT INTO (creates objects)
            if select.into.is_some() {
                return Err(anyhow!("SELECT INTO is not allowed"));
            }

            for table in &select.from {
                validate_table_with_joins(table, cte_names, depth)?;
            }

            for item in &select.projection {
                if let sqlparser::ast::SelectItem::UnnamedExpr(expr)
                | sqlparser::ast::SelectItem::ExprWithAlias { expr, .. } = item
                {
                    validate_expr(expr, cte_names, depth)?;
                }
            }

            if let Some(selection) = &select.selection {
                validate_expr(selection, cte_names, depth)?;
            }

            // Validate GROUP BY expressions
            if let sqlparser::ast::GroupByExpr::Expressions(exprs, _) = &select.group_by {
                for expr in exprs {
                    validate_expr(expr, cte_names, depth)?;
                }
            }

            // Validate HAVING
            if let Some(having) = &select.having {
                validate_expr(having, cte_names, depth)?;
            }

            Ok(())
        }
        SetExpr::Query(q) => validate_query_ast(q, cte_names, depth),
        SetExpr::SetOperation { left, right, .. } => {
            validate_set_expr(left, cte_names, depth)?;
            validate_set_expr(right, cte_names, depth)
        }
        SetExpr::Values(values) => {
            for row in &values.rows {
                for expr in row {
                    validate_expr(expr, cte_names, depth)?;
                }
            }
            Ok(())
        }
        SetExpr::Insert(_) => Err(anyhow!("INSERT not allowed")),
        SetExpr::Update(_) => Err(anyhow!("UPDATE not allowed")),
        SetExpr::Delete(_) => Err(anyhow!("DELETE not allowed")),
        SetExpr::Merge(_) => Err(anyhow!("MERGE not allowed")),
        SetExpr::Table(_) => Err(anyhow!("TABLE statement is not allowed")),
    }
}

fn validate_table_with_joins(
    table: &TableWithJoins,
    cte_names: &HashSet<String>,
    depth: usize,
) -> Result<()> {
    validate_table_factor(&table.relation, cte_names, depth)?;
    for join in &table.joins {
        validate_table_factor(&join.relation, cte_names, depth)?;
        let constraint = match &join.join_operator {
            sqlparser::ast::JoinOperator::Join(c)
            | sqlparser::ast::JoinOperator::Inner(c)
            | sqlparser::ast::JoinOperator::Left(c)
            | sqlparser::ast::JoinOperator::LeftOuter(c)
            | sqlparser::ast::JoinOperator::Right(c)
            | sqlparser::ast::JoinOperator::RightOuter(c)
            | sqlparser::ast::JoinOperator::FullOuter(c)
            | sqlparser::ast::JoinOperator::CrossJoin(c)
            | sqlparser::ast::JoinOperator::Semi(c)
            | sqlparser::ast::JoinOperator::LeftSemi(c)
            | sqlparser::ast::JoinOperator::RightSemi(c)
            | sqlparser::ast::JoinOperator::Anti(c)
            | sqlparser::ast::JoinOperator::LeftAnti(c)
            | sqlparser::ast::JoinOperator::RightAnti(c) => Some(c),
            _ => None,
        };
        if let Some(sqlparser::ast::JoinConstraint::On(expr)) = constraint {
            validate_expr(expr, cte_names, depth)?;
        }
    }
    Ok(())
}

fn validate_table_factor(
    factor: &TableFactor,
    cte_names: &HashSet<String>,
    depth: usize,
) -> Result<()> {
    match factor {
        TableFactor::Table { name, args, .. } => {
            if args.is_some() {
                return Err(anyhow!("Table functions are not allowed"));
            }
            validate_table_name(name, cte_names)
        }
        TableFactor::Derived { subquery, .. } => validate_query_ast(subquery, cte_names, depth + 1),
        TableFactor::TableFunction { .. } => Err(anyhow!("Table functions are not allowed")),
        TableFactor::Function { .. } => Err(anyhow!("Table functions are not allowed")),
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => validate_table_with_joins(table_with_joins, cte_names, depth),
        _ => Err(anyhow!("Unsupported FROM clause type")),
    }
}

fn validate_table_name(name: &ObjectName, cte_names: &HashSet<String>) -> Result<()> {
    let full_name = name.to_string().to_lowercase();

    const BLOCKED_SCHEMAS: &[&str] = &["pg_catalog", "information_schema", "pg_temp", "pg_toast"];

    for schema in BLOCKED_SCHEMAS {
        if full_name.starts_with(schema) {
            return Err(anyhow!(
                "Access to system catalog '{schema}' is not allowed"
            ));
        }
    }

    const BLOCKED_TABLES: &[&str] = &[
        "pg_stat_activity",
        "pg_settings",
        "pg_user",
        "pg_shadow",
        "pg_authid",
        "pg_roles",
    ];

    for table in BLOCKED_TABLES {
        if full_name.contains(table) {
            return Err(anyhow!("Access to table '{table}' is not allowed"));
        }
    }

    let bare_name = name
        .0
        .last()
        .and_then(|part| part.as_ident())
        .map(|ident| ident.value.to_lowercase())
        .unwrap_or_default();

    if ALLOWED_TABLES.contains(&bare_name.as_str()) {
        return Ok(());
    }

    if cte_names.contains(&bare_name) {
        return Ok(());
    }

    Err(anyhow!("Access to table '{bare_name}' is not allowed"))
}

/// Reject-by-default expression validation.
/// Only explicitly allowed expression types are permitted.
fn validate_expr(expr: &Expr, cte_names: &HashSet<String>, depth: usize) -> Result<()> {
    match expr {
        // Safe leaf nodes
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => Ok(()),
        Expr::Value(_) => Ok(()),
        Expr::TypedString(_) => Ok(()),
        Expr::Wildcard(_) | Expr::QualifiedWildcard(_, _) => Ok(()),

        // Function calls (validated against allowlist)
        Expr::Function(func) => validate_function(func, cte_names, depth),

        // Subqueries (increment depth)
        Expr::Subquery(q) => validate_query_ast(q, cte_names, depth + 1),
        Expr::InSubquery { expr, subquery, .. } => {
            validate_expr(expr, cte_names, depth)?;
            validate_query_ast(subquery, cte_names, depth + 1)
        }
        Expr::Exists { subquery, .. } => validate_query_ast(subquery, cte_names, depth + 1),

        // Binary / unary operations
        Expr::BinaryOp { left, right, .. } => {
            validate_expr(left, cte_names, depth)?;
            validate_expr(right, cte_names, depth)
        }
        Expr::UnaryOp { expr, .. } => validate_expr(expr, cte_names, depth),

        // Range expressions
        Expr::Between {
            expr, low, high, ..
        } => {
            validate_expr(expr, cte_names, depth)?;
            validate_expr(low, cte_names, depth)?;
            validate_expr(high, cte_names, depth)
        }

        // CASE WHEN
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            if let Some(op) = operand {
                validate_expr(op, cte_names, depth)?;
            }
            for case_when in conditions {
                validate_expr(&case_when.condition, cte_names, depth)?;
                validate_expr(&case_when.result, cte_names, depth)?;
            }
            if let Some(else_r) = else_result {
                validate_expr(else_r, cte_names, depth)?;
            }
            Ok(())
        }

        // Type casting
        Expr::Cast { expr, .. } => validate_expr(expr, cte_names, depth),
        Expr::Nested(e) => validate_expr(e, cte_names, depth),

        // IN list
        Expr::InList { expr, list, .. } => {
            validate_expr(expr, cte_names, depth)?;
            for item in list {
                validate_expr(item, cte_names, depth)?;
            }
            Ok(())
        }

        // Boolean tests
        Expr::IsNull(e)
        | Expr::IsNotNull(e)
        | Expr::IsTrue(e)
        | Expr::IsFalse(e)
        | Expr::IsNotTrue(e)
        | Expr::IsNotFalse(e)
        | Expr::IsUnknown(e)
        | Expr::IsNotUnknown(e) => validate_expr(e, cte_names, depth),

        // Pattern matching
        Expr::Like { expr, pattern, .. } | Expr::ILike { expr, pattern, .. } => {
            validate_expr(expr, cte_names, depth)?;
            validate_expr(pattern, cte_names, depth)
        }
        Expr::SimilarTo { expr, pattern, .. } => {
            validate_expr(expr, cte_names, depth)?;
            validate_expr(pattern, cte_names, depth)
        }

        // ANY/ALL operators
        Expr::AnyOp { left, right, .. } | Expr::AllOp { left, right, .. } => {
            validate_expr(left, cte_names, depth)?;
            validate_expr(right, cte_names, depth)
        }

        // IS DISTINCT FROM
        Expr::IsDistinctFrom(a, b) | Expr::IsNotDistinctFrom(a, b) => {
            validate_expr(a, cte_names, depth)?;
            validate_expr(b, cte_names, depth)
        }

        // SQL builtins parsed as dedicated Expr variants (not Function)
        Expr::Extract { expr, .. } => validate_expr(expr, cte_names, depth),
        Expr::Substring {
            expr,
            substring_from,
            substring_for,
            ..
        } => {
            validate_expr(expr, cte_names, depth)?;
            if let Some(from) = substring_from {
                validate_expr(from, cte_names, depth)?;
            }
            if let Some(for_expr) = substring_for {
                validate_expr(for_expr, cte_names, depth)?;
            }
            Ok(())
        }
        Expr::Trim {
            expr, trim_what, ..
        } => {
            validate_expr(expr, cte_names, depth)?;
            if let Some(what) = trim_what {
                validate_expr(what, cte_names, depth)?;
            }
            Ok(())
        }
        Expr::Ceil { expr, .. } | Expr::Floor { expr, .. } => validate_expr(expr, cte_names, depth),
        Expr::Position { expr, r#in, .. } => {
            validate_expr(expr, cte_names, depth)?;
            validate_expr(r#in, cte_names, depth)
        }
        Expr::Overlay {
            expr,
            overlay_what,
            overlay_from,
            overlay_for,
            ..
        } => {
            validate_expr(expr, cte_names, depth)?;
            validate_expr(overlay_what, cte_names, depth)?;
            validate_expr(overlay_from, cte_names, depth)?;
            if let Some(for_expr) = overlay_for {
                validate_expr(for_expr, cte_names, depth)?;
            }
            Ok(())
        }
        Expr::Collate { expr, .. } => validate_expr(expr, cte_names, depth),
        Expr::AtTimeZone {
            timestamp,
            time_zone,
            ..
        } => {
            validate_expr(timestamp, cte_names, depth)?;
            validate_expr(time_zone, cte_names, depth)
        }

        // Tuple / row constructors
        Expr::Tuple(exprs) => {
            for e in exprs {
                validate_expr(e, cte_names, depth)?;
            }
            Ok(())
        }

        // Array literal
        Expr::Array(arr) => {
            for e in &arr.elem {
                validate_expr(e, cte_names, depth)?;
            }
            Ok(())
        }

        // Interval literal
        Expr::Interval(_) => Ok(()),

        // Reject everything else (reject-by-default)
        _ => Err(anyhow!("Unsupported expression type")),
    }
}

pub const ALLOWED_FUNCTIONS: &[&str] = &[
    // ABI decode helpers (custom PostgreSQL functions)
    "abi_uint",
    "abi_int",
    "abi_address",
    "abi_bool",
    "abi_bytes",
    "abi_string",
    "format_address",
    "format_uint",
    "topic_addr",
    // ClickHouse decode helpers emitted by the `?signature=` CTE generator
    // (`to_cte_sql_clickhouse*`). Pure byte-reinterpretation, read-only;
    // needed so the CH public path can run this same validator on the
    // wrapped SQL. Lowercase: the validator lowercases names before lookup.
    "unhex",
    "reinterpretasuint256",
    "reinterpretasint256",
    // Aggregates
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "array_agg",
    "string_agg",
    "bool_and",
    "bool_or",
    // Scalar / null handling
    "coalesce",
    "nullif",
    "greatest",
    "least",
    // Numeric
    "abs",
    "round",
    "floor",
    "ceil",
    "ceiling",
    "trunc",
    "pow",
    "power",
    "mod",
    "sign",
    // String
    "lower",
    "upper",
    "length",
    "substring",
    "substr",
    "trim",
    "ltrim",
    "rtrim",
    "replace",
    "concat",
    "concat_ws",
    "left",
    "right",
    "lpad",
    "rpad",
    "position",
    "strpos",
    "starts_with",
    "repeat",
    "reverse",
    "to_hex",
    // Bytea / hex
    "encode",
    "decode",
    "octet_length",
    // Bytea
    "bit_length",
    // Time
    "date",
    "date_trunc",
    "date_part",
    "extract",
    "to_timestamp",
    "to_char",
    "now",
    "age",
    // Window functions
    "row_number",
    "rank",
    "dense_rank",
    "lag",
    "lead",
    "first_value",
    "last_value",
    "ntile",
    "percent_rank",
    "cume_dist",
    // Type casting helpers
    "cast",
];

fn is_allowed_function(name: &str) -> bool {
    let bare_name = name.rsplit('.').next().unwrap_or(name);
    ALLOWED_FUNCTIONS.contains(&bare_name)
}

fn validate_function(func: &Function, cte_names: &HashSet<String>, depth: usize) -> Result<()> {
    let func_name = func.name.to_string().to_lowercase();

    if !is_allowed_function(&func_name) {
        return Err(anyhow!("Function '{}' is not allowed", func_name));
    }

    if let FunctionArguments::List(arg_list) = &func.args {
        for arg in &arg_list.args {
            if let FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))
            | FunctionArg::Named {
                arg: FunctionArgExpr::Expr(expr),
                ..
            } = arg
            {
                validate_expr(expr, cte_names, depth)?;
            }
        }
    }

    // Validate FILTER (WHERE ...) clause
    if let Some(filter) = &func.filter {
        validate_expr(filter, cte_names, depth)?;
    }

    // Validate WITHIN GROUP (ORDER BY ...) clause
    for order_expr in &func.within_group {
        validate_expr(&order_expr.expr, cte_names, depth)?;
    }

    // Validate window function OVER clause
    if let Some(sqlparser::ast::WindowType::WindowSpec(spec)) = &func.over {
        for expr in &spec.partition_by {
            validate_expr(expr, cte_names, depth)?;
        }
        for order_expr in &spec.order_by {
            validate_expr(&order_expr.expr, cte_names, depth)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_select() {
        assert!(validate_query("SELECT * FROM blocks").is_ok());
        assert!(validate_query("SELECT num, hash FROM blocks WHERE num > 100").is_ok());
    }

    #[test]
    fn test_select_with_cte() {
        assert!(validate_query("WITH t AS (SELECT * FROM blocks) SELECT * FROM t").is_ok());
    }

    #[test]
    fn test_rejects_multiple_statements() {
        assert!(validate_query("SELECT 1; SELECT 2").is_err());
    }

    #[test]
    fn test_rejects_insert() {
        assert!(validate_query("INSERT INTO blocks VALUES (1)").is_err());
    }

    #[test]
    fn test_rejects_update() {
        assert!(validate_query("UPDATE blocks SET num = 1").is_err());
    }

    #[test]
    fn test_rejects_delete() {
        assert!(validate_query("DELETE FROM blocks").is_err());
    }

    #[test]
    fn test_rejects_data_modifying_cte() {
        let result = validate_query(
            "WITH x AS (UPDATE blocks SET miner = 'pwn' RETURNING 1) SELECT * FROM x",
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_rejects_comment_bypass() {
        let result = validate_query(
            "WITH x AS (UPDA/**/TE blocks SET miner = 'pwn' RETURNING 1) SELECT * FROM x",
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_rejects_pg_sleep() {
        assert!(validate_query("SELECT pg_sleep(5)").is_err());
    }

    #[test]
    fn test_rejects_pg_read_file() {
        assert!(validate_query("SELECT pg_read_file('/etc/passwd')").is_err());
    }

    #[test]
    fn test_rejects_file_table_function() {
        assert!(validate_query("SELECT * FROM file('/etc/passwd')").is_err());
    }

    #[test]
    fn test_rejects_url_table_function() {
        assert!(validate_query("SELECT * FROM url('http://example.com/data.csv')").is_err());
    }

    #[test]
    fn test_rejects_s3_table_function() {
        assert!(validate_query("SELECT * FROM s3('s3://bucket/file.parquet')").is_err());
    }

    #[test]
    fn test_rejects_system_catalog() {
        assert!(validate_query("SELECT * FROM pg_catalog.pg_tables").is_err());
        assert!(validate_query("SELECT * FROM information_schema.tables").is_err());
    }

    #[test]
    fn test_allows_aggregate_functions() {
        assert!(validate_query("SELECT COUNT(*), SUM(gas_used) FROM blocks").is_ok());
    }

    #[test]
    fn test_allows_window_functions() {
        assert!(validate_query("SELECT num, ROW_NUMBER() OVER (ORDER BY num) FROM blocks").is_ok());
    }

    #[test]
    fn test_allows_subquery() {
        assert!(
            validate_query("SELECT * FROM blocks WHERE num IN (SELECT block_num FROM txs)").is_ok()
        );
    }

    #[test]
    fn test_rejects_nested_dangerous_function() {
        assert!(validate_query("SELECT COALESCE(pg_sleep(1), 0)").is_err());
    }

    #[test]
    fn test_rejects_sync_state() {
        assert!(validate_query("SELECT * FROM sync_state").is_err());
    }

    #[test]
    fn test_allows_internal_txs() {
        // Phase 4: callTracer-derived nested calls.
        assert!(validate_query("SELECT * FROM internal_txs").is_ok());
        assert!(
            validate_query(
                "SELECT depth, call_type, \"from\", \"to\" FROM internal_txs WHERE tx_hash = decode('aa', 'hex')"
            )
            .is_ok()
        );
    }

    #[test]
    fn test_rejects_pg_tables() {
        assert!(validate_query("SELECT * FROM pg_tables").is_err());
    }

    #[test]
    fn test_rejects_unknown_table() {
        assert!(validate_query("SELECT * FROM some_random_table").is_err());
    }

    #[test]
    fn test_allows_cte_defined_table() {
        assert!(
            validate_query("WITH my_cte AS (SELECT * FROM blocks) SELECT * FROM my_cte").is_ok()
        );
    }

    #[test]
    fn test_rejects_dblink() {
        assert!(
            validate_query(
                "SELECT * FROM dblink('host=evil dbname=secrets', 'SELECT * FROM passwords')"
            )
            .is_err()
        );
        assert!(validate_query("SELECT dblink_connect('myconn', 'host=evil')").is_err());
        assert!(validate_query("SELECT dblink_exec('myconn', 'DROP TABLE blocks')").is_err());
    }

    #[test]
    fn test_allows_schema_qualified_tables() {
        assert!(validate_query("SELECT * FROM public.blocks").is_ok());
    }

    #[test]
    fn test_rejects_recursive_cte() {
        assert!(
            validate_query(
                "WITH RECURSIVE r AS (SELECT 1 AS n UNION ALL SELECT n+1 FROM r) SELECT * FROM r"
            )
            .is_err()
        );
    }

    #[test]
    fn test_rejects_generate_series() {
        assert!(validate_query("SELECT generate_series(1, 1000000000)").is_err());
        assert!(
            validate_query(
                "SELECT * FROM blocks WHERE num IN (SELECT generate_series(1, 1000000))"
            )
            .is_err()
        );
    }

    #[test]
    fn test_rejects_values_function_bypass() {
        assert!(validate_query("VALUES (pg_sleep(10))").is_err());
        assert!(validate_query("VALUES (pg_read_file('/etc/passwd'))").is_err());
    }

    #[test]
    fn test_rejects_table_statement() {
        assert!(validate_query("TABLE blocks").is_err());
        assert!(validate_query("TABLE pg_shadow").is_err());
    }

    #[test]
    fn test_rejects_select_into() {
        assert!(validate_query("SELECT * INTO newtable FROM blocks").is_err());
    }

    #[test]
    fn test_rejects_lo_functions() {
        assert!(validate_query("SELECT lo_get(12345)").is_err());
        assert!(validate_query("SELECT lo_open(12345, 262144)").is_err());
    }

    #[test]
    fn test_rejects_admin_file_functions() {
        assert!(validate_query("SELECT pg_file_read('/etc/passwd', 0, 1000)").is_err());
        assert!(validate_query("SELECT pg_file_write('/tmp/evil', 'data', false)").is_err());
    }

    #[test]
    fn test_rejects_dangerous_function_in_having() {
        assert!(
            validate_query(
                "SELECT COUNT(*) FROM blocks GROUP BY num HAVING pg_sleep(1) IS NOT NULL"
            )
            .is_err()
        );
    }

    #[test]
    fn test_rejects_dangerous_function_in_join_on() {
        assert!(
            validate_query("SELECT * FROM blocks JOIN txs ON pg_sleep(1) IS NOT NULL").is_err()
        );
    }

    #[test]
    fn test_allows_simple_values() {
        assert!(validate_query("VALUES (1, 'hello'), (2, 'world')").is_ok());
    }

    #[test]
    fn test_rejects_unknown_function() {
        assert!(validate_query("SELECT md5('test') FROM blocks").is_err());
        assert!(validate_query("SELECT regexp_replace(hash, 'a', 'b') FROM blocks").is_err());
    }

    #[test]
    fn test_allows_abi_helpers() {
        assert!(validate_query("SELECT abi_uint(input) FROM txs").is_ok());
        assert!(validate_query("SELECT abi_address(input) FROM txs").is_ok());
        assert!(validate_query("SELECT format_address(miner) FROM blocks").is_ok());
    }

    #[test]
    fn test_allows_common_functions() {
        assert!(validate_query("SELECT COALESCE(gas_used, 0) FROM blocks").is_ok());
        assert!(validate_query("SELECT ABS(gas_used) FROM blocks").is_ok());
        assert!(validate_query("SELECT LOWER('test') FROM blocks").is_ok());
        assert!(validate_query("SELECT date_trunc('hour', to_timestamp(ts)) FROM blocks").is_ok());
    }

    #[test]
    fn test_rejects_all_table_functions() {
        assert!(validate_query("SELECT * FROM generate_series(1, 100)").is_err());
        assert!(validate_query("SELECT * FROM unnest(ARRAY[1,2,3])").is_err());
    }

    #[test]
    fn test_rejects_unsupported_table_factor() {
        assert!(validate_query("SELECT * FROM UNNEST(ARRAY[1,2,3])").is_err());
    }

    // === New tests for this commit ===

    #[test]
    fn test_rejects_for_update() {
        assert!(validate_query("SELECT * FROM blocks FOR UPDATE").is_err());
        assert!(validate_query("SELECT * FROM blocks FOR SHARE").is_err());
    }

    #[test]
    fn test_rejects_dangerous_function_in_order_by() {
        assert!(validate_query("SELECT * FROM blocks ORDER BY pg_sleep(1)").is_err());
    }

    #[test]
    fn test_rejects_excessive_limit() {
        assert!(validate_query("SELECT * FROM blocks LIMIT 100000000").is_err());
        assert!(validate_query("SELECT * FROM blocks LIMIT 10001").is_err());
    }

    #[test]
    fn test_allows_reasonable_limit() {
        assert!(validate_query("SELECT * FROM blocks LIMIT 100").is_ok());
        assert!(validate_query("SELECT * FROM blocks LIMIT 10000").is_ok());
        assert!(validate_query("SELECT * FROM blocks LIMIT 1 OFFSET 5").is_ok());
    }

    #[test]
    fn test_rejects_subquery_in_limit() {
        assert!(validate_query("SELECT * FROM blocks LIMIT (SELECT 1)").is_err());
    }

    #[test]
    fn test_rejects_deep_subquery_nesting() {
        // 5 levels of derived table nesting exceeds MAX_SUBQUERY_DEPTH (4)
        let deep = "SELECT * FROM (SELECT * FROM (SELECT * FROM (SELECT * FROM (SELECT * FROM (SELECT * FROM blocks) a) b) c) d) e";
        assert!(validate_query(deep).is_err());
    }

    #[test]
    fn test_allows_moderate_subquery_nesting() {
        // 3 levels of nesting is within limits
        let moderate = "SELECT * FROM (SELECT * FROM (SELECT * FROM blocks) a) b";
        assert!(validate_query(moderate).is_ok());
    }

    #[test]
    fn test_rejects_query_too_large() {
        let huge = format!(
            "SELECT * FROM blocks WHERE num IN ({})",
            "1,".repeat(70_000)
        );
        assert!(validate_query(&huge).is_err());
    }

    #[test]
    fn test_allows_order_by_column() {
        assert!(validate_query("SELECT * FROM blocks ORDER BY num DESC").is_ok());
        assert!(validate_query("SELECT * FROM blocks ORDER BY num DESC, hash ASC").is_ok());
    }

    #[test]
    fn test_allows_cast_expression() {
        assert!(validate_query("SELECT CAST(num AS TEXT) FROM blocks").is_ok());
    }

    #[test]
    fn test_rejects_catalog_lookup_casts() {
        // 2026-11-07: a prober used '<name>'::regclass / ::regprocedure to test
        // whether any table or function exists, bypassing the table allowlist.
        for sql in [
            "SELECT 'pg_authid'::regclass::text AS t",
            "SELECT 'pg_read_file(text)'::regprocedure::text AS t",
            "SELECT CAST('pg_class' AS regclass) AS t",
            "SELECT regclass 'pg_class' AS t",
            "SELECT 'pg_class'::pg_catalog.regclass AS t",
            "SELECT 'pg_class'::\"regclass\" AS t",
            "SELECT '{pg_class}'::regclass[] AS t",
            "SELECT 'int4'::regtype AS t",
            "SELECT 'postgres'::regrole AS t",
            "SELECT 'public'::regnamespace AS t",
            "SELECT 'now'::regproc AS t",
            "SELECT '+'::regoper AS t",
            "SELECT 'english'::regconfig AS t",
            "SELECT num FROM blocks WHERE 'pg_authid'::regclass IS NOT NULL",
            "SELECT 1 AS y WHERE ('pg_authid'::regclass)::text = ANY (SELECT 'x')",
            "SELECT to_regclass('pg_authid') AS t",
        ] {
            assert!(validate_query(sql).is_err(), "should reject: {sql}");
        }
    }

    #[test]
    fn test_allows_ordinary_casts() {
        for sql in [
            "SELECT num::text FROM blocks",
            "SELECT '1'::numeric AS n",
            "SELECT CAST(num AS BIGINT) FROM blocks",
            "SELECT num::varchar(20) FROM blocks",
            "SELECT timestamp::date FROM blocks",
            "SELECT '{1,2}'::int[] AS a",
            "SELECT date '2026-01-01' AS d",
            // Quoted literals inside a type are not type names.
            "SELECT CAST(timestamp AS DateTime64(3, 'America/Regina')) AS t FROM blocks",
            "SELECT CAST('a' AS Enum8('regular' = 1, 'other' = 2)) AS e",
            // Field names are not type names.
            "SELECT CAST(num AS Tuple(region String, registered UInt8)) AS t FROM blocks",
            "SELECT CAST(num AS Nullable(String)) AS t FROM blocks",
        ] {
            assert!(validate_query(sql).is_ok(), "should allow: {sql}");
        }
    }

    #[test]
    fn test_allows_between() {
        assert!(validate_query("SELECT * FROM blocks WHERE num BETWEEN 1 AND 100").is_ok());
    }

    #[test]
    fn test_allows_like() {
        assert!(validate_query("SELECT * FROM txs WHERE hash LIKE '%abc%'").is_ok());
    }

    #[test]
    fn test_allows_is_null() {
        assert!(validate_query("SELECT * FROM blocks WHERE miner IS NOT NULL").is_ok());
    }

    #[test]
    fn test_allows_case_when() {
        assert!(
            validate_query("SELECT CASE WHEN num > 100 THEN 'big' ELSE 'small' END FROM blocks")
                .is_ok()
        );
    }

    #[test]
    fn test_allows_array_literal() {
        assert!(validate_query("SELECT * FROM blocks WHERE num = ANY(ARRAY[1,2,3])").is_ok());
    }

    #[test]
    fn test_rejects_filter_clause_bypass() {
        assert!(
            validate_query("SELECT COUNT(*) FILTER (WHERE pg_sleep(1) IS NOT NULL) FROM blocks")
                .is_err()
        );
    }

    #[test]
    fn test_rejects_limit_null() {
        assert!(validate_query("SELECT * FROM blocks LIMIT NULL").is_err());
    }

    #[test]
    fn test_rejects_negative_limit() {
        assert!(validate_query("SELECT * FROM blocks LIMIT -1").is_err());
    }

    #[test]
    fn test_rejects_fetch_clause() {
        assert!(validate_query("SELECT * FROM blocks FETCH FIRST 10 ROWS ONLY").is_err());
    }

    // === engine-aware row cap (item #8C) ===

    #[test]
    fn engine_cap_is_higher_for_clickhouse_than_postgres() {
        assert_eq!(engine_limit_cap(Some("clickhouse")), HARD_LIMIT_CLICKHOUSE);
        assert!(HARD_LIMIT_CLICKHOUSE > HARD_LIMIT_MAX);
    }

    #[test]
    fn engine_cap_defaults_to_postgres_for_unknown_or_missing_engine() {
        assert_eq!(engine_limit_cap(None), HARD_LIMIT_MAX);
        assert_eq!(engine_limit_cap(Some("postgres")), HARD_LIMIT_MAX);
        assert_eq!(engine_limit_cap(Some("")), HARD_LIMIT_MAX);
        assert_eq!(engine_limit_cap(Some("duckdb")), HARD_LIMIT_MAX); // historical, no longer supported
        assert_eq!(engine_limit_cap(Some("garbage")), HARD_LIMIT_MAX);
    }

    #[test]
    fn engine_cap_is_case_sensitive() {
        // "ClickHouse" or "CLICKHOUSE" should NOT raise the cap — only the
        // exact lowercase form the API documents. Otherwise typos silently
        // change behavior.
        assert_eq!(engine_limit_cap(Some("ClickHouse")), HARD_LIMIT_MAX);
        assert_eq!(engine_limit_cap(Some("CLICKHOUSE")), HARD_LIMIT_MAX);
    }

    // 2026-11-06 incident: public /query requests that passed validation and ran
    // pg_read_file as the Postgres superuser, and url() in ClickHouse. The
    // forbidden call sat on the LEFT of `= ANY(...)`, which was never checked.
    #[test]
    fn rejects_attacker_queries_from_2026_11_06() {
        for sql in [
            "select 1 as y where (pg_read_file('/etc/hostname')) = ANY(ARRAY['x'])",
            "select 1 as y where (cast((select (pg_read_file('/tmp/agent13.pid'))) as int)) = ANY(ARRAY[0])",
            "select 1 as y where (cast((select (pg_read_file('/tmp/agent91.pid', 0, 180))) as int)) = ANY(ARRAY[0])",
            "SELECT 1 FROM logs WHERE (SELECT s FROM url('http://127.0.0.1:8123/?query=SELECT%201', 'RawBLOB', 's String')) = ANY(ARRAY['x'])",
        ] {
            assert!(validate_query(sql).is_err(), "must reject: {sql}");
        }
    }

    // The public Postgres path drops to the `tidx_query` role with SET LOCAL ROLE;
    // set_config('role', ...) would switch it back to the superuser session role.
    #[test]
    fn rejects_role_and_setting_changes() {
        for sql in [
            "SELECT set_config('role', 'tidx', true)",
            "SELECT 1 WHERE set_config('role', 'tidx', true) = ANY(ARRAY['tidx'])",
            "SELECT set_config('session_preload_libraries', 'x', false)",
        ] {
            assert!(validate_query(sql).is_err(), "must reject: {sql}");
        }
    }

    #[test]
    fn rejects_forbidden_function_left_of_all() {
        assert!(validate_query("SELECT 1 WHERE pg_read_file('/x') = ALL(ARRAY['a'])").is_err());
    }

    #[test]
    fn allows_plain_any_and_all_comparisons() {
        validate_query("SELECT num FROM blocks WHERE num = ANY(ARRAY[1, 2])").unwrap();
        validate_query("SELECT num FROM blocks WHERE num > ALL(ARRAY[1, 2])").unwrap();
    }

    // Defense in depth: a forbidden function hidden in any expression position
    // must be rejected, so one missed match arm can't reopen the hole.
    #[test]
    fn rejects_forbidden_function_in_every_expression_position() {
        let f = "pg_read_file('/x')";
        for sql in [
            format!("SELECT 1 WHERE {f} = ANY(ARRAY['a'])"),
            format!("SELECT 1 WHERE 'a' = ANY(ARRAY[{f}])"),
            format!("SELECT ARRAY[{f}]"),
            format!("SELECT ({f})[1]"),
            format!("SELECT ({f}, 1)"),
            format!("SELECT ROW({f})"),
            format!("SELECT CAST({f} AS text)"),
            format!("SELECT {f}::text"),
            format!("SELECT {f} COLLATE \"C\""),
            format!("SELECT {f} AT TIME ZONE 'UTC'"),
            format!("SELECT SUBSTRING({f} FROM 1 FOR 2)"),
            format!("SELECT TRIM({f})"),
            format!("SELECT POSITION('a' IN {f})"),
            format!("SELECT EXTRACT(YEAR FROM {f}::timestamp)"),
            format!("SELECT CASE WHEN {f} = 'a' THEN 1 END"),
            format!("SELECT 1 FROM blocks ORDER BY {f}"),
            format!("SELECT 1 FROM blocks GROUP BY {f}"),
            format!("SELECT count(*) FROM blocks HAVING {f} = 'a'"),
            format!("SELECT num FROM blocks WHERE EXISTS (SELECT {f})"),
            format!("SELECT num FROM blocks WHERE num IN (SELECT length({f}))"),
            format!("WITH t AS (SELECT {f} AS v) SELECT v FROM t"),
            format!("SELECT num FROM blocks UNION SELECT length({f})"),
            format!("SELECT {f} IS DISTINCT FROM 'a'"),
            format!("SELECT {f} LIKE 'a%'"),
            format!("SELECT {f} BETWEEN 'a' AND 'b'"),
        ] {
            assert!(validate_query(&sql).is_err(), "must reject: {sql}");
        }
    }

    // Clauses the first pass does not walk must not smuggle in other tables.
    #[test]
    fn rejects_disallowed_tables_in_rarely_used_clauses() {
        let sub = "(SELECT count(*) FROM pg_authid)";
        for sql in [
            format!("SELECT DISTINCT ON ({sub}) num FROM blocks"),
            format!("SELECT num FROM blocks QUALIFY {sub} > 0"),
            format!("SELECT num FROM blocks WINDOW w AS (ORDER BY {sub})"),
            format!("SELECT num FROM blocks PREWHERE {sub} > 0"),
        ] {
            assert!(validate_query(&sql).is_err(), "must reject: {sql}");
        }
    }

    #[test]
    fn rejects_settings_and_format_clauses() {
        for sql in [
            "SELECT num FROM blocks SETTINGS max_threads = 1",
            "SELECT num FROM blocks FORMAT JSON",
        ] {
            assert!(validate_query(sql).is_err(), "must reject: {sql}");
        }
    }

    // ---- lexing: both parsers must agree where strings/comments end ----
    // 2026-11-06 review: ClickHouse reads `\'` inside '...' as an escaped quote,
    // sqlparser (GenericDialect) as a backslash followed by the closing quote.

    #[test]
    fn clickhouse_rejects_tokens_its_lexer_reads_differently() {
        for sql in [
            r"SELECT num FROM blocks WHERE hash = 'a\'b'",
            r"SELECT num FROM blocks WHERE hash = 'a\\b'",
            r#"SELECT "a\"b" FROM blocks"#,
            "SELECT num FROM blocks # comment",
            "SELECT $$x$$",
            "SELECT num FROM blocks /* a /* b */ c */",
        ] {
            assert!(
                reject_ambiguous_lexing(sql, SqlEngine::ClickHouse).is_err(),
                "must reject: {sql}"
            );
        }
    }

    #[test]
    fn postgres_rejects_escape_unicode_and_dollar_strings() {
        for sql in [
            r"SELECT E'a\'b'",
            r"SELECT e'x'",
            r"SELECT U&'d\0061t'",
            // Code review of #45: sqlparser reads this as a cast to type U, AND,
            // an identifier; Postgres reads a cast to regclass.
            r#"SELECT 'pg_authid'::U&"\0072egclass"::text AS t"#,
            r#"SELECT 'pg_authid'::u&"\0072egclass" AS t"#,
            r#"SELECT U&"\0070g_authid".x FROM blocks"#,
            "SELECT $$x$$",
            "SELECT $q$x$q$",
            "SELECT 1 /* a /* b */ c */",
        ] {
            assert!(
                reject_ambiguous_lexing(sql, SqlEngine::Postgres).is_err(),
                "must reject: {sql}"
            );
        }
    }

    #[test]
    fn ordinary_sql_passes_the_lexing_check() {
        for engine in [SqlEngine::Postgres, SqlEngine::ClickHouse] {
            for sql in [
                "SELECT num FROM blocks WHERE hash = '0xabc' AND note = 'it''s'",
                "SELECT \"num\" FROM blocks -- trailing comment",
                "SELECT num FROM blocks /* note */ WHERE num > 1",
                "SELECT num FROM blocks WHERE miner LIKE'a%'",
                "SELECT num FROM blocks WHERE type = 'e'",
                r#"SELECT 'U&"x"' AS t"#,
                r#"SELECT num & "num" FROM blocks"#,
            ] {
                reject_ambiguous_lexing(sql, engine).unwrap_or_else(|e| panic!("{sql}: {e}"));
            }
        }
        // Postgres keeps standard_conforming_strings on, so '\x..' bytea text is
        // a literal backslash for both parsers and stays allowed there.
        reject_ambiguous_lexing(r"SELECT num FROM blocks WHERE hash = '\xab'", SqlEngine::Postgres)
            .unwrap();
    }
}
