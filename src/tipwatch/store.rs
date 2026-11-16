use anyhow::Result;

use crate::db::Pool;

/// Tables, created on first start inside the `tipwatch` schema.
pub const SCHEMA_SQL: &str = include_str!("../../db/tipwatch.sql");
/// One-time superuser setup: the `tidx_tipwatch` role and its schema.
pub const ROLE_SQL: &str = include_str!("../../db/tipwatch_role.sql");

/// Creates the tables if any is missing. Runs no DDL when they all exist, so a
/// restart does not show up as DDL in the Postgres log.
pub async fn ensure_schema(pool: &Pool) -> Result<()> {
    let _ = pool;
    todo!()
}
