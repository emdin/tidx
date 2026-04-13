use anyhow::Result;
use tracing::{info, warn};

use super::Pool;

pub async fn run_migrations(pool: &Pool) -> Result<()> {
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
        let user: String = session.get(1);

        match conn
            .execute("SELECT pg_terminate_backend($1)", &[&pid])
            .await
        {
            Ok(_) => terminated += 1,
            Err(error) => {
                skipped += 1;
                warn!(pid, user = %user, error = %error, "Could not terminate existing database session before migrations");
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
    conn.batch_execute(include_str!("../../db/sync_state.sql"))
        .await?;
    conn.batch_execute(include_str!("../../db/explorer.sql"))
        .await?;
    conn.batch_execute(include_str!("../../db/functions.sql"))
        .await?;

    // Load any optional extensions
    conn.batch_execute(include_str!("../../db/extensions.sql"))
        .await?;

    Ok(())
}
