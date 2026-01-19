mod partitions;
mod pool;
mod schema;

pub use partitions::PartitionManager;
pub use pool::create_pool;
pub use schema::run_migrations;

pub type Pool = deadpool_postgres::Pool;
