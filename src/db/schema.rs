use anyhow::Result;
use tracing::info;

use super::Pool;

pub async fn run_migrations(pool: &Pool) -> Result<()> {
    let conn = pool.get().await?;

    info!("Running TimescaleDB hypertable migrations");
    conn.batch_execute(include_str!(
        "../../migrations/006_timescale_hypertables.sql"
    ))
    .await?;

    Ok(())
}


