use anyhow::Result;
use tracing::{info, warn};

use super::Pool;

/// Whether the pool's role is a superuser. The engine should connect as the
/// ordinary owner role from `db/app_role.sql`: as a superuser, any /query
/// validator gap is file access and code execution (2026-11-06).
pub async fn connected_as_superuser(pool: &Pool) -> Result<bool> {
    let conn = pool.get().await?;
    let row = conn
        .query_one(
            "SELECT rolsuper FROM pg_roles WHERE rolname = current_user",
            &[],
        )
        .await?;
    Ok(row.get(0))
}

pub async fn run_migrations(pool: &Pool) -> Result<()> {
    if connected_as_superuser(pool).await? {
        warn!(
            "Connected to Postgres as a superuser; run db/app_role.sql and point pg_url at tidx_app"
        );
    }
    let conn = pool.get().await?;

    // Best-effort cleanup of other connections before running migrations.
    // We cannot always terminate admin/superuser sessions from the app role,
    // so failures are logged and migrations continue.
    let sessions = conn
        .query(
            r#"
            SELECT pid, usename
            FROM pg_stat_activity
            WHERE pid != pg_backend_pid()
              AND datname = current_database()
            "#,
            &[],
        )
        .await?;

    let mut terminated = 0usize;
    let mut skipped = 0usize;

    for session in sessions {
        let pid: i32 = session.get(0);
        // NULL for background sessions (autovacuum / parallel workers) attached
        // to this database; reading it as `String` panicked at boot whenever
        // one happened to be active.
        let user: Option<String> = session.get(1);

        match conn
            .execute("SELECT pg_terminate_backend($1)", &[&pid])
            .await
        {
            Ok(_) => terminated += 1,
            Err(error) => {
                skipped += 1;
                warn!(pid, user = user.as_deref().unwrap_or("<background>"), error = %error, "Could not terminate existing database session before migrations");
            }
        }
    }

    if terminated > 0 {
        warn!(
            count = terminated,
            "Terminated stale connections before migrations"
        );
    }
    if skipped > 0 {
        warn!(
            count = skipped,
            "Skipped non-terminable sessions before migrations"
        );
    }

    info!("Running schema migrations");
    conn.batch_execute(include_str!("../../db/blocks.sql"))
        .await?;
    conn.batch_execute(include_str!("../../db/txs.sql")).await?;
    conn.batch_execute(include_str!("../../db/logs.sql"))
        .await?;
    conn.batch_execute(include_str!("../../db/receipts.sql"))
        .await?;
    conn.batch_execute(include_str!("../../db/l2_withdrawals.sql"))
        .await?;
    conn.batch_execute(include_str!("../../db/internal_txs.sql"))
        .await?;
    conn.batch_execute(include_str!("../../db/trace_outcomes.sql"))
        .await?;
    conn.batch_execute(include_str!("../../db/sync_state.sql"))
        .await?;
    conn.batch_execute(include_str!("../../db/explorer.sql"))
        .await?;
    conn.batch_execute(include_str!("../../db/kaspa_provenance.sql"))
        .await?;
    conn.batch_execute(include_str!("../../db/reorg_archive.sql"))
        .await?;
    conn.batch_execute(include_str!("../../db/functions.sql"))
        .await?;

    // Load any optional extensions
    conn.batch_execute(include_str!("../../db/extensions.sql"))
        .await?;

    // Last, so its GRANT ... ON ALL TABLES covers every table created above.
    conn.batch_execute(include_str!("../../db/query_role.sql"))
        .await?;

    drop(conn);
    // Fail fast at boot if a source-table migration has drifted from its
    // orphaned_* twin: the reorg handler would corrupt at 3 a.m. otherwise.
    crate::sync::reorg_archive::assert_schema_parity(pool).await?;

    Ok(())
}
