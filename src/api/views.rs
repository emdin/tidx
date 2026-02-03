//! Views API for managing ClickHouse materialized views

use axum::{
    extract::{ConnectInfo, Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

use super::{AppState, ApiError};
use crate::query::EventSignature;

/// Validate view name (alphanumeric + underscore only)
fn is_valid_view_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Deserialize)]
pub struct ChainQuery {
    #[serde(alias = "chain_id")]
    #[serde(rename = "chainId")]
    chain_id: u64,
}

#[derive(Serialize)]
pub struct ColumnInfo {
    name: String,
    #[serde(rename = "type")]
    col_type: String,
}

#[derive(Serialize)]
pub struct ViewInfo {
    database: String,
    engine: String,
    name: String,
    columns: Vec<ColumnInfo>,
}

#[derive(Serialize)]
pub struct ListViewsResponse {
    ok: bool,
    views: Vec<ViewInfo>,
}

/// GET /views?chainId=42431 - List all views
pub async fn list_views(
    State(state): State<AppState>,
    Query(params): Query<ChainQuery>,
) -> Result<Json<ListViewsResponse>, ApiError> {
    let clickhouse = state
        .get_clickhouse(Some(params.chain_id))
        .await
        .ok_or_else(|| ApiError::BadRequest(format!(
            "ClickHouse not configured for chain_id: {}",
            params.chain_id
        )))?;

    let database = format!("analytics_{}", params.chain_id);
    
    // Query system.tables for views in analytics database
    let sql = format!(
        "SELECT name, engine FROM system.tables WHERE database = '{}' AND engine IN ('View', 'MaterializedView') ORDER BY name",
        database
    );

    let result = clickhouse.query(&sql, None).await
        .map_err(|e| ApiError::QueryError(e.to_string()))?;

    let mut views = Vec::new();
    for row in &result.rows {
        let name = row.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let engine = row.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
        
        // Get columns for this view
        let columns_sql = format!(
            "SELECT name, type FROM system.columns WHERE database = '{}' AND table = '{}' ORDER BY position",
            database, name
        );
        let columns_result = clickhouse.query(&columns_sql, None).await
            .map_err(|e| ApiError::QueryError(e.to_string()))?;
        
        let columns: Vec<ColumnInfo> = columns_result.rows.iter().map(|col_row| {
            ColumnInfo {
                name: col_row.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                col_type: col_row.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }
        }).collect();
        
        views.push(ViewInfo {
            name,
            engine,
            database: database.clone(),
            columns,
        });
    }

    Ok(Json(ListViewsResponse { ok: true, views }))
}

#[derive(Deserialize)]
pub struct CreateViewRequest {
    #[serde(alias = "chain_id")]
    #[serde(rename = "chainId")]
    chain_id: u64,
    #[serde(default = "default_engine")]
    engine: String,
    name: String,
    #[serde(rename = "orderBy")]
    order_by: Vec<String>,
    /// Optional event signature for automatic CTE generation and decoding.
    /// E.g., "Transfer(address indexed from, address indexed to, uint256 value)"
    signature: Option<String>,
    sql: String,
}

fn default_engine() -> String {
    "SummingMergeTree()".to_string()
}

#[derive(Serialize)]
pub struct CreateViewResponse {
    backfill_rows: u64,
    ok: bool,
    view: ViewInfo,
}

/// POST /views - Create a materialized view (trusted IP required)
pub async fn create_view(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<CreateViewRequest>,
) -> Result<Json<CreateViewResponse>, ApiError> {
    // Check trusted IP access
    if !state.is_trusted_ip(&addr) {
        return Err(ApiError::Forbidden("Mutations only allowed from trusted IPs".to_string()));
    }

    // Validate view name
    if !is_valid_view_name(&req.name) {
        return Err(ApiError::BadRequest("Invalid view name: must be alphanumeric with underscores".to_string()));
    }

    // Validate order_by
    if req.order_by.is_empty() {
        return Err(ApiError::BadRequest("orderBy is required".to_string()));
    }

    // Validate SQL is SELECT only
    let sql_upper = req.sql.trim().to_uppercase();
    if !sql_upper.starts_with("SELECT") {
        return Err(ApiError::BadRequest("SQL must be a SELECT statement".to_string()));
    }

    // Parse signature if provided
    let signature = if let Some(ref sig_str) = req.signature {
        Some(EventSignature::parse(sig_str)
            .map_err(|e| ApiError::BadRequest(format!("Invalid signature: {}", e)))?)
    } else {
        None
    };

    let clickhouse = state
        .get_clickhouse(Some(req.chain_id))
        .await
        .ok_or_else(|| ApiError::BadRequest(format!(
            "ClickHouse not configured for chain_id: {}",
            req.chain_id
        )))?;

    let database = format!("analytics_{}", req.chain_id);
    let table_name = &req.name;
    let mv_name = format!("{}_mv", req.name);
    let order_by = req.order_by.join(", ");

    // Rewrite table references in SQL to include database prefix
    let sql = super::rewrite_analytics_tables(&req.sql, req.chain_id);

    // If signature provided, generate CTE with decoded columns and apply predicate pushdown
    let sql = if let Some(ref sig) = signature {
        let sql = sig.rewrite_filters_for_pushdown(&sql);
        let cte = sig.to_cte_sql_clickhouse();
        format!("WITH {} {}", cte, sql)
    } else {
        sql
    };

    // 1. Ensure database exists
    let create_db = format!("CREATE DATABASE IF NOT EXISTS {}", database);
    clickhouse.query(&create_db, None).await
        .map_err(|e| ApiError::QueryError(format!("Failed to create database: {}", e)))?;

    // 2. Create target table (infer schema from SELECT ... LIMIT 0)
    let create_table = format!(
        "CREATE TABLE IF NOT EXISTS {}.{} ENGINE = {} ORDER BY ({}) AS {} LIMIT 0",
        database, table_name, req.engine, order_by, sql
    );
    clickhouse.query(&create_table, None).await
        .map_err(|e| ApiError::QueryError(format!("Failed to create table: {}", e)))?;

    // 3. Create materialized view
    let create_mv = format!(
        "CREATE MATERIALIZED VIEW IF NOT EXISTS {}.{} TO {}.{} AS {}",
        database, mv_name, database, table_name, sql
    );
    clickhouse.query(&create_mv, None).await
        .map_err(|e| ApiError::QueryError(format!("Failed to create materialized view: {}", e)))?;

    // 4. Backfill existing data
    let backfill = format!(
        "INSERT INTO {}.{} {}",
        database, table_name, sql
    );
    clickhouse.query(&backfill, None).await
        .map_err(|e| ApiError::QueryError(format!("Failed to backfill: {}", e)))?;

    // 5. Get row count
    let count_sql = format!("SELECT count() FROM {}.{}", database, table_name);
    let count_result = clickhouse.query(&count_sql, None).await
        .map_err(|e| ApiError::QueryError(format!("Failed to get count: {}", e)))?;
    
    let backfill_rows = count_result.rows
        .first()
        .and_then(|r| r.first())
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    Ok(Json(CreateViewResponse {
        ok: true,
        view: ViewInfo {
            name: table_name.clone(),
            engine: "MaterializedView".to_string(),
            database,
            columns: vec![],
        },
        backfill_rows,
    }))
}

#[derive(Serialize)]
pub struct DeleteViewResponse {
    deleted: Vec<String>,
    ok: bool,
}

/// DELETE /views/{name}?chainId=42431 - Delete a view (trusted IP required)
pub async fn delete_view(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(name): Path<String>,
    Query(params): Query<ChainQuery>,
) -> Result<Json<DeleteViewResponse>, ApiError> {
    // Check trusted IP access
    if !state.is_trusted_ip(&addr) {
        return Err(ApiError::Forbidden("Mutations only allowed from trusted IPs".to_string()));
    }

    // Validate view name
    if !is_valid_view_name(&name) {
        return Err(ApiError::BadRequest("Invalid view name".to_string()));
    }

    let clickhouse = state
        .get_clickhouse(Some(params.chain_id))
        .await
        .ok_or_else(|| ApiError::BadRequest(format!(
            "ClickHouse not configured for chain_id: {}",
            params.chain_id
        )))?;

    let database = format!("analytics_{}", params.chain_id);
    let mv_name = format!("{}_mv", name);
    let mut deleted = Vec::new();

    // Drop MV first
    let drop_mv = format!("DROP VIEW IF EXISTS {}.{}", database, mv_name);
    if clickhouse.query(&drop_mv, None).await.is_ok() {
        deleted.push(mv_name);
    }

    // Drop target table
    let drop_table = format!("DROP TABLE IF EXISTS {}.{}", database, name);
    if clickhouse.query(&drop_table, None).await.is_ok() {
        deleted.push(name);
    }

    Ok(Json(DeleteViewResponse { ok: true, deleted }))
}

#[derive(Serialize)]
pub struct GetViewResponse {
    definition: String,
    ok: bool,
    row_count: u64,
    view: ViewInfo,
}

/// GET /views/{name}?chainId=42431 - Get view details
pub async fn get_view(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(params): Query<ChainQuery>,
) -> Result<Json<GetViewResponse>, ApiError> {
    let clickhouse = state
        .get_clickhouse(Some(params.chain_id))
        .await
        .ok_or_else(|| ApiError::BadRequest(format!(
            "ClickHouse not configured for chain_id: {}",
            params.chain_id
        )))?;

    let database = format!("analytics_{}", params.chain_id);

    // Get view definition
    let sql = format!(
        "SELECT engine, create_table_query FROM system.tables WHERE database = '{}' AND name = '{}'",
        database, name
    );
    let result = clickhouse.query(&sql, None).await
        .map_err(|e| ApiError::QueryError(e.to_string()))?;

    if result.rows.is_empty() {
        return Err(ApiError::NotFound(format!("View '{}' not found", name)));
    }

    let row = &result.rows[0];
    let engine = row.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let definition = row.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();

    // Get row count
    let count_sql = format!("SELECT count() FROM {}.{}", database, name);
    let count_result = clickhouse.query(&count_sql, None).await.ok();
    let row_count = count_result
        .and_then(|r| r.rows.first().cloned())
        .and_then(|r| r.first().cloned())
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    Ok(Json(GetViewResponse {
        ok: true,
        view: ViewInfo {
            name,
            engine,
            database,
            columns: vec![],
        },
        definition,
        row_count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_view_name() {
        assert!(is_valid_view_name("token_holders"));
        assert!(is_valid_view_name("my_view_123"));
        assert!(is_valid_view_name("View1"));
        
        assert!(!is_valid_view_name(""));
        assert!(!is_valid_view_name("123view")); // Starts with number
        assert!(!is_valid_view_name("my-view")); // Has hyphen
        assert!(!is_valid_view_name("my view")); // Has space
    }

    // ========================================================================
    // Signature CTE Generation Tests for Views
    // ========================================================================

    #[test]
    fn test_signature_generates_cte_for_transfer() {
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        let user_sql = r#"SELECT "to", SUM("value") as total FROM Transfer GROUP BY "to""#;
        let cte = sig.to_cte_sql_clickhouse();
        let full_sql = format!("WITH {} {}", cte, user_sql);
        
        // CTE should include decoded columns
        assert!(full_sql.contains("AS \"from\""));
        assert!(full_sql.contains("AS \"to\""));
        assert!(full_sql.contains("AS \"value\""));
        
        // CTE should have proper ClickHouse decode functions
        assert!(full_sql.contains("concat('0x', lower(substring("));  // address decode
        assert!(full_sql.contains("reinterpretAsUInt256"));           // uint256 decode
        
        // User's query is preserved
        assert!(full_sql.contains(r#"SELECT "to", SUM("value") as total"#));
    }

    #[test]
    fn test_signature_generates_cte_for_swap() {
        let sig = EventSignature::parse(
            "Swap(address indexed sender, uint256 amount0In, uint256 amount1In, uint256 amount0Out, uint256 amount1Out, address indexed to)"
        ).unwrap();
        
        let cte = sig.to_cte_sql_clickhouse();
        
        // Indexed params: sender (topic1), to (topic2)
        assert!(cte.contains("AS \"sender\""));
        assert!(cte.contains("AS \"to\""));
        
        // Non-indexed data params
        assert!(cte.contains("AS \"amount0In\""));
        assert!(cte.contains("AS \"amount1In\""));
        assert!(cte.contains("AS \"amount0Out\""));
        assert!(cte.contains("AS \"amount1Out\""));
    }

    #[test]
    fn test_signature_with_bool_param() {
        let sig = EventSignature::parse("Paused(bool paused)").unwrap();
        let cte = sig.to_cte_sql_clickhouse();
        
        // Bool decode uses unhex comparison
        assert!(cte.contains("unhex("));
        assert!(cte.contains("!= unhex('00')"));
        assert!(cte.contains("AS \"paused\""));
    }

    #[test]
    fn test_signature_with_bytes32_indexed() {
        let sig = EventSignature::parse(
            "RoleGranted(bytes32 indexed role, address indexed account, address indexed sender)"
        ).unwrap();
        
        let cte = sig.to_cte_sql_clickhouse();
        
        // bytes32 indexed is passed through as hex
        assert!(cte.contains("AS \"role\""));
        assert!(cte.contains("AS \"account\""));
        assert!(cte.contains("AS \"sender\""));
    }

    #[test]
    fn test_signature_with_int256() {
        let sig = EventSignature::parse("PriceUpdate(int256 price)").unwrap();
        let cte = sig.to_cte_sql_clickhouse();
        
        // int256 uses reinterpretAsInt256
        assert!(cte.contains("reinterpretAsInt256"));
        assert!(cte.contains("AS \"price\""));
    }

    #[test]
    fn test_signature_predicate_pushdown_in_view() {
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        // User query with filter on decoded column
        let user_sql = r#"SELECT "value" FROM Transfer WHERE "from" = '0xdAC17F958D2ee523a2206206994597C13D831ec7'"#;
        
        // Apply pushdown
        let rewritten = sig.rewrite_filters_for_pushdown(user_sql);
        
        // Should rewrite to topic1 with left-padded address
        assert!(rewritten.contains("topic1 = '0x000000000000000000000000dac17f958d2ee523a2206206994597c13d831ec7'"));
        assert!(!rewritten.contains(r#""from" ="#));
    }

    #[test]
    fn test_signature_multiple_filters_pushdown() {
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        let user_sql = r#"SELECT "value" FROM Transfer WHERE "from" = '0xdAC17F958D2ee523a2206206994597C13D831ec7' AND "to" = '0xa726a1CD723409074DF9108A2187cfA19899aCF8'"#;
        let rewritten = sig.rewrite_filters_for_pushdown(user_sql);
        
        // Both should be rewritten
        assert!(rewritten.contains("topic1 = '0x000000000000000000000000dac17f958d2ee523a2206206994597c13d831ec7'"));
        assert!(rewritten.contains("topic2 = '0x000000000000000000000000a726a1cd723409074df9108a2187cfa19899acf8'"));
    }

    #[test]
    fn test_signature_non_indexed_not_pushed_down() {
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        // "value" is not indexed - should not be rewritten
        let user_sql = r#"SELECT * FROM Transfer WHERE "value" > 1000000"#;
        let rewritten = sig.rewrite_filters_for_pushdown(user_sql);
        
        // Should remain unchanged (no equality filter on value anyway)
        assert_eq!(user_sql, rewritten);
    }

    #[test]
    fn test_signature_invalid_address_not_pushed_down() {
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        // Invalid address (too short) - should not be rewritten
        let user_sql = r#"SELECT * FROM Transfer WHERE "from" = '0xabc'"#;
        let rewritten = sig.rewrite_filters_for_pushdown(user_sql);
        
        // Should remain unchanged
        assert_eq!(user_sql, rewritten);
    }

    #[test]
    fn test_signature_parse_error() {
        // Missing closing paren
        let result = EventSignature::parse("Transfer(address indexed from");
        assert!(result.is_err());
        
        // Empty name
        let result = EventSignature::parse("(address from)");
        assert!(result.is_err());
        
        // Invalid type
        let result = EventSignature::parse("Transfer(invalid_type from)");
        assert!(result.is_err());
    }

    #[test]
    fn test_cte_selector_filter() {
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        let cte = sig.to_cte_sql_clickhouse();
        
        // CTE should filter by selector (topic0)
        // Transfer selector: 0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef
        assert!(cte.contains("selector ="));
        assert!(cte.contains("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"));
    }

    #[test]
    fn test_cte_exposes_raw_columns() {
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        let cte = sig.to_cte_sql_clickhouse();
        
        // CTE should expose raw columns for filtering
        assert!(cte.contains("topic1"));
        assert!(cte.contains("topic2"));
        assert!(cte.contains("topic3"));
        assert!(cte.contains("data"));
        assert!(cte.contains("selector"));
    }

    #[test]
    fn test_full_view_sql_generation() {
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        // Simulate what create_view does
        let user_sql = r#"SELECT "to", COUNT(*) as cnt, SUM("value") as total FROM Transfer WHERE "from" = '0xdAC17F958D2ee523a2206206994597C13D831ec7' GROUP BY "to""#;
        
        // Step 1: Predicate pushdown
        let sql = sig.rewrite_filters_for_pushdown(user_sql);
        
        // Step 2: Add CTE
        let cte = sig.to_cte_sql_clickhouse();
        let full_sql = format!("WITH {} {}", cte, sql);
        
        // Verify complete SQL has all components
        assert!(full_sql.starts_with("WITH transfer AS"));
        assert!(full_sql.contains("topic1 = '0x000000000000000000000000dac17f958d2ee523a2206206994597c13d831ec7'")); // Pushed down filter
        assert!(full_sql.contains("AS \"to\"")); // Decoded column
        assert!(full_sql.contains("AS \"value\"")); // Decoded column
        assert!(full_sql.contains("GROUP BY \"to\"")); // User's GROUP BY preserved
    }

    #[test]
    fn test_approval_event_signature() {
        let sig = EventSignature::parse(
            "Approval(address indexed owner, address indexed spender, uint256 value)"
        ).unwrap();
        
        let cte = sig.to_cte_sql_clickhouse();
        
        // Check all params are decoded
        assert!(cte.contains("AS \"owner\""));
        assert!(cte.contains("AS \"spender\""));
        assert!(cte.contains("AS \"value\""));
        
        // Check correct topic assignments
        assert!(cte.contains("topic1")); // owner
        assert!(cte.contains("topic2")); // spender
    }

    #[test]
    fn test_unnamed_params_get_arg_names() {
        let sig = EventSignature::parse(
            "Transfer(address indexed, address indexed, uint256)"
        ).unwrap();
        
        let cte = sig.to_cte_sql_clickhouse();
        
        // Unnamed params should get arg0, arg1, arg2
        assert!(cte.contains("AS \"arg0\""));
        assert!(cte.contains("AS \"arg1\""));
        assert!(cte.contains("AS \"arg2\""));
    }

    #[test]
    fn test_mixed_indexed_and_data_params() {
        // Deposit(address indexed dst, uint256 wad)
        // dst is indexed (topic1), wad is in data
        let sig = EventSignature::parse(
            "Deposit(address indexed dst, uint256 wad)"
        ).unwrap();
        
        let cte = sig.to_cte_sql_clickhouse();
        
        // dst from topic1
        assert!(cte.contains("topic1"));
        assert!(cte.contains("AS \"dst\""));
        
        // wad from data (first 32 bytes)
        assert!(cte.contains("AS \"wad\""));
        assert!(cte.contains("substring(data,")); // Data decode
    }

    // ========================================================================
    // Complex Real-World View Tests
    // ========================================================================

    #[test]
    fn test_token_holders_view() {
        // Token holders view: tracks balance per (token, address)
        // Needs to sum incoming transfers and subtract outgoing
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        // This is a complex query that creates a holder balance view
        // It unions incoming (+value) and outgoing (-value) transfers
        let user_sql = r#"
            SELECT 
                address as token,
                holder,
                SUM(delta) as balance
            FROM (
                SELECT address, "to" as holder, CAST("value" AS Int256) as delta FROM Transfer
                UNION ALL
                SELECT address, "from" as holder, -CAST("value" AS Int256) as delta FROM Transfer
            )
            GROUP BY token, holder
            HAVING balance > 0
        "#;
        
        let sql = sig.rewrite_filters_for_pushdown(user_sql);
        let cte = sig.to_cte_sql_clickhouse();
        let full_sql = format!("WITH {} {}", cte, sql);
        
        // Verify CTE has all required decoded columns
        assert!(full_sql.contains("AS \"from\""));
        assert!(full_sql.contains("AS \"to\""));
        assert!(full_sql.contains("AS \"value\""));
        
        // Verify user query structure preserved
        assert!(full_sql.contains("UNION ALL"));
        assert!(full_sql.contains("GROUP BY token, holder"));
        assert!(full_sql.contains("HAVING balance > 0"));
    }

    #[test]
    fn test_token_supply_view() {
        // Token supply view: tracks total supply per token
        // Supply = sum of mints (from = 0x0) - sum of burns (to = 0x0)
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        let user_sql = r#"
            SELECT 
                address as token,
                SUM(CASE 
                    WHEN "from" = '0x0000000000000000000000000000000000000000' THEN CAST("value" AS Int256)
                    WHEN "to" = '0x0000000000000000000000000000000000000000' THEN -CAST("value" AS Int256)
                    ELSE 0
                END) as supply
            FROM Transfer
            GROUP BY token
        "#;
        
        let sql = sig.rewrite_filters_for_pushdown(user_sql);
        let cte = sig.to_cte_sql_clickhouse();
        let full_sql = format!("WITH {} {}", cte, sql);
        
        // Verify structure
        assert!(full_sql.contains("AS \"from\""));
        assert!(full_sql.contains("AS \"to\""));
        assert!(full_sql.contains("AS \"value\""));
        assert!(full_sql.contains("CASE"));
        assert!(full_sql.contains("GROUP BY token"));
    }

    #[test]
    fn test_transfer_count_per_address_view() {
        // Count transfers per address (both sent and received)
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        let user_sql = r#"
            SELECT 
                addr,
                SUM(sent) as total_sent,
                SUM(received) as total_received,
                SUM(sent) + SUM(received) as total_transfers
            FROM (
                SELECT "from" as addr, 1 as sent, 0 as received FROM Transfer
                UNION ALL
                SELECT "to" as addr, 0 as sent, 1 as received FROM Transfer
            )
            GROUP BY addr
            ORDER BY total_transfers DESC
        "#;
        
        let cte = sig.to_cte_sql_clickhouse();
        let full_sql = format!("WITH {} {}", cte, user_sql);
        
        assert!(full_sql.contains("AS \"from\""));
        assert!(full_sql.contains("AS \"to\""));
        assert!(full_sql.contains("UNION ALL"));
        assert!(full_sql.contains("ORDER BY total_transfers DESC"));
    }

    #[test]
    fn test_uniswap_swap_volume_view() {
        // Uniswap V2 swap volume aggregation
        let sig = EventSignature::parse(
            "Swap(address indexed sender, uint256 amount0In, uint256 amount1In, uint256 amount0Out, uint256 amount1Out, address indexed to)"
        ).unwrap();
        
        let user_sql = r#"
            SELECT 
                address as pair,
                toStartOfHour(block_timestamp) as hour,
                COUNT(*) as swap_count,
                SUM("amount0In") + SUM("amount0Out") as volume0,
                SUM("amount1In") + SUM("amount1Out") as volume1
            FROM Swap
            GROUP BY pair, hour
            ORDER BY hour DESC
        "#;
        
        let cte = sig.to_cte_sql_clickhouse();
        let full_sql = format!("WITH {} {}", cte, user_sql);
        
        // Verify all data params are decoded
        assert!(full_sql.contains("AS \"sender\""));
        assert!(full_sql.contains("AS \"amount0In\""));
        assert!(full_sql.contains("AS \"amount1In\""));
        assert!(full_sql.contains("AS \"amount0Out\""));
        assert!(full_sql.contains("AS \"amount1Out\""));
        assert!(full_sql.contains("AS \"to\""));
        
        // Verify aggregation structure
        assert!(full_sql.contains("toStartOfHour"));
        assert!(full_sql.contains("GROUP BY pair, hour"));
    }

    #[test]
    fn test_approval_allowances_view() {
        // Track current allowances from Approval events (last approval wins)
        let sig = EventSignature::parse(
            "Approval(address indexed owner, address indexed spender, uint256 value)"
        ).unwrap();
        
        let user_sql = r#"
            SELECT 
                address as token,
                "owner",
                "spender",
                argMax("value", block_num) as current_allowance
            FROM Approval
            GROUP BY token, "owner", "spender"
        "#;
        
        let cte = sig.to_cte_sql_clickhouse();
        let full_sql = format!("WITH {} {}", cte, user_sql);
        
        assert!(full_sql.contains("AS \"owner\""));
        assert!(full_sql.contains("AS \"spender\""));
        assert!(full_sql.contains("AS \"value\""));
        assert!(full_sql.contains("argMax"));
    }

    #[test]
    fn test_filtered_token_holders_view() {
        // Token holders for a specific token address
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        // Filter by specific token contract using address column
        let user_sql = r#"
            SELECT 
                "to" as holder,
                SUM(CAST("value" AS Int256)) as balance
            FROM Transfer
            WHERE address = '0xdAC17F958D2ee523a2206206994597C13D831ec7'
            GROUP BY holder
            HAVING balance > 0
        "#;
        
        let cte = sig.to_cte_sql_clickhouse();
        let full_sql = format!("WITH {} {}", cte, user_sql);
        
        // Address filter preserved (not a decoded column, no pushdown)
        assert!(full_sql.contains("address = '0xdAC17F958D2ee523a2206206994597C13D831ec7'"));
        assert!(full_sql.contains("AS \"to\""));
        assert!(full_sql.contains("AS \"value\""));
    }

    #[test]
    fn test_daily_transfer_stats_view() {
        // Daily transfer statistics
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        let user_sql = r#"
            SELECT 
                toDate(block_timestamp) as day,
                address as token,
                COUNT(*) as transfer_count,
                COUNT(DISTINCT "from") as unique_senders,
                COUNT(DISTINCT "to") as unique_receivers,
                SUM("value") as total_volume,
                AVG("value") as avg_transfer_size
            FROM Transfer
            GROUP BY day, token
            ORDER BY day DESC, total_volume DESC
        "#;
        
        let cte = sig.to_cte_sql_clickhouse();
        let full_sql = format!("WITH {} {}", cte, user_sql);
        
        assert!(full_sql.contains("AS \"from\""));
        assert!(full_sql.contains("AS \"to\""));
        assert!(full_sql.contains("AS \"value\""));
        assert!(full_sql.contains("toDate(block_timestamp)"));
        assert!(full_sql.contains("COUNT(DISTINCT"));
    }

    #[test]
    fn test_whale_transfers_view() {
        // Large transfers (whales) - filters on non-indexed value column
        let sig = EventSignature::parse(
            "Transfer(address indexed from, address indexed to, uint256 value)"
        ).unwrap();
        
        let user_sql = r#"
            SELECT 
                block_num,
                block_timestamp,
                address as token,
                "from",
                "to",
                "value"
            FROM Transfer
            WHERE "value" > 1000000000000000000000
            ORDER BY block_num DESC
        "#;
        
        let sql = sig.rewrite_filters_for_pushdown(user_sql);
        let cte = sig.to_cte_sql_clickhouse();
        let full_sql = format!("WITH {} {}", cte, sql);
        
        // value filter should NOT be pushed down (not indexed, not equality)
        assert!(full_sql.contains("\"value\" > 1000000000000000000000"));
        assert!(full_sql.contains("AS \"from\""));
        assert!(full_sql.contains("AS \"to\""));
        assert!(full_sql.contains("AS \"value\""));
    }
}
