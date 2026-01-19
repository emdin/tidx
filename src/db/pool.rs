use anyhow::Result;
use deadpool_postgres::{Config, Pool, Runtime};
use tokio_postgres::NoTls;

pub async fn create_pool(database_url: &str) -> Result<Pool> {
    let mut config = Config::new();
    config.url = Some(database_url.to_string());

    let pool = config.create_pool(Some(Runtime::Tokio1), NoTls)?;

    // Test connection
    let _ = pool.get().await?;

    Ok(pool)
}
