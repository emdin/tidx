use alloy::primitives::B256;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tokio_postgres::Transaction;

use super::rpc::Header;
use crate::db::Pool;

/// Tables, created on first start inside the `tipwatch` schema.
pub const SCHEMA_SQL: &str = include_str!("../../db/tipwatch.sql");
/// One-time superuser setup: the `tidx_tipwatch` role and its schema.
pub const ROLE_SQL: &str = include_str!("../../db/tipwatch_role.sql");

const TABLES: [&str; 4] = [
    "tipwatch.seen",
    "tipwatch.reorgs",
    "tipwatch.orphans",
    "tipwatch.gaps",
];

/// A block as the watcher saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seen {
    pub header: Header,
    pub seen_at: DateTime<Utc>,
}

/// SCHEMA_SQL without its comment lines. With log_statement=ddl the whole batch
/// is logged as one statement, and the host's security audit only recognises
/// idempotent schema DDL that starts with `CREATE ... IF NOT EXISTS`.
fn schema_sql_to_run() -> String {
    SCHEMA_SQL
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Creates the tables if any is missing. Runs no DDL when they all exist, so a
/// restart does not show up as DDL in the Postgres log.
pub async fn ensure_schema(pool: &Pool) -> Result<()> {
    let conn = pool.get().await?;
    let present: i64 = conn
        .query_one(
            "SELECT count(*) FROM unnest($1::text[]) t WHERE to_regclass(t) IS NOT NULL",
            &[&TABLES.to_vec()],
        )
        .await?
        .get(0);
    if present < TABLES.len() as i64 {
        conn.batch_execute(&schema_sql_to_run())
            .await
            .context("creating tipwatch tables (has db/tipwatch_role.sql been run?)")?;
    }
    Ok(())
}

fn ts(secs: u64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs as i64, 0).unwrap_or(DateTime::UNIX_EPOCH)
}

fn row_to_seen(r: &tokio_postgres::Row) -> Seen {
    let block_ts: DateTime<Utc> = r.get(3);
    Seen {
        header: Header {
            number: r.get::<_, i64>(0) as u64,
            hash: B256::from_slice(r.get::<_, &[u8]>(1)),
            parent_hash: B256::from_slice(r.get::<_, &[u8]>(2)),
            timestamp: block_ts.timestamp() as u64,
        },
        seen_at: r.get(4),
    }
}

/// The provider's most recent `limit` heights, oldest first.
pub async fn load_recent(pool: &Pool, provider: &str, limit: usize) -> Result<Vec<Seen>> {
    let conn = pool.get().await?;
    let rows = conn
        .query(
            "SELECT num, hash, parent_hash, block_ts, seen_at FROM tipwatch.seen
             WHERE provider = $1 ORDER BY num DESC LIMIT $2",
            &[&provider, &(limit as i64)],
        )
        .await?;
    Ok(rows.iter().rev().map(row_to_seen).collect())
}

pub struct NewReorg<'a> {
    pub fork_point: u64,
    pub old_tip: u64,
    pub new_tip: u64,
    pub orphaned: &'a [Seen],
    /// New-branch blocks above the fork that were fetched (all of them unless
    /// the reorg was deeper than --max-walk).
    pub added: &'a [Seen],
    pub after_gap: bool,
}

/// One step's changes, written in a single transaction.
pub enum Op<'a> {
    Extend(&'a [Seen]),
    Reorg(NewReorg<'a>),
    /// Heights `from..=to` were not observed one by one; `seen` restarts from
    /// `head`, so it always matches the watcher's memory.
    Gap {
        from: u64,
        to: u64,
        reason: &'a str,
        head: Seen,
    },
}

/// Applies the ops in order in one transaction and returns the new reorg ids.
pub async fn commit(pool: &Pool, provider: &str, ops: &[Op<'_>]) -> Result<Vec<i64>> {
    let mut conn = pool.get().await?;
    let tx = conn.transaction().await?;
    let mut ids = Vec::new();
    for op in ops {
        match op {
            Op::Extend(blocks) => upsert_seen(&tx, provider, blocks).await?,
            Op::Reorg(r) => ids.push(insert_reorg(&tx, provider, r).await?),
            Op::Gap {
                from,
                to,
                reason,
                head,
            } => {
                tx.execute(
                    "INSERT INTO tipwatch.gaps (provider, from_num, to_num, reason) VALUES ($1, $2, $3, $4)",
                    &[&provider, &(*from as i64), &(*to as i64), reason],
                )
                .await?;
                tx.execute(
                    "DELETE FROM tipwatch.seen WHERE provider = $1",
                    &[&provider],
                )
                .await?;
                upsert_seen(&tx, provider, std::slice::from_ref(head)).await?;
            }
        }
    }
    tx.commit().await?;
    Ok(ids)
}

async fn upsert_seen(tx: &Transaction<'_>, provider: &str, blocks: &[Seen]) -> Result<()> {
    let stmt = tx
        .prepare(
            "INSERT INTO tipwatch.seen (provider, num, hash, parent_hash, block_ts, seen_at)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (provider, num) DO UPDATE SET hash = EXCLUDED.hash,
                 parent_hash = EXCLUDED.parent_hash, block_ts = EXCLUDED.block_ts,
                 seen_at = EXCLUDED.seen_at",
        )
        .await?;
    for b in blocks {
        let h = &b.header;
        tx.execute(
            &stmt,
            &[
                &provider,
                &(h.number as i64),
                &h.hash.as_slice(),
                &h.parent_hash.as_slice(),
                &ts(h.timestamp),
                &b.seen_at,
            ],
        )
        .await?;
    }
    Ok(())
}

/// Records the reorg and its orphans, marks earlier reorgs whose orphans came
/// back, and replaces the provider's seen chain above the fork point.
async fn insert_reorg(tx: &Transaction<'_>, provider: &str, r: &NewReorg<'_>) -> Result<i64> {
    let depth = i32::try_from(r.orphaned.len()).context("reorg depth overflows i32")?;
    let id: i64 = tx
        .query_one(
            "INSERT INTO tipwatch.reorgs (provider, fork_point, old_tip, new_tip, depth, after_gap)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            &[
                &provider,
                &(r.fork_point as i64),
                &(r.old_tip as i64),
                &(r.new_tip as i64),
                &depth,
                &r.after_gap,
            ],
        )
        .await?
        .get(0);

    let stmt = tx
        .prepare(
            "INSERT INTO tipwatch.orphans (reorg_id, provider, num, hash, parent_hash, block_ts, seen_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .await?;
    for o in r.orphaned {
        let h = &o.header;
        tx.execute(
            &stmt,
            &[
                &id,
                &provider,
                &(h.number as i64),
                &h.hash.as_slice(),
                &h.parent_hash.as_slice(),
                &ts(h.timestamp),
                &o.seen_at,
            ],
        )
        .await?;
    }

    let returned: Vec<&[u8]> = r.added.iter().map(|b| b.header.hash.as_slice()).collect();
    tx.execute(
        "UPDATE tipwatch.reorgs SET reverted_by = $1
         WHERE provider = $2 AND reverted_by IS NULL AND id <> $1
           AND id IN (SELECT reorg_id FROM tipwatch.orphans WHERE provider = $2 AND hash = ANY($3))",
        &[&id, &provider, &returned],
    )
    .await?;

    tx.execute(
        "DELETE FROM tipwatch.seen WHERE provider = $1 AND num > $2",
        &[&provider, &(r.fork_point as i64)],
    )
    .await?;
    upsert_seen(tx, provider, r.added).await?;
    Ok(id)
}

/// Deletes the provider's seen rows at or below `upto`.
pub async fn prune_seen(pool: &Pool, provider: &str, upto: u64) -> Result<u64> {
    let conn = pool.get().await?;
    Ok(conn
        .execute(
            "DELETE FROM tipwatch.seen WHERE provider = $1 AND num <= $2",
            &[&provider, &(upto as i64)],
        )
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_ddl_is_sent_without_comments() {
        let sql = schema_sql_to_run();
        assert!(
            sql.starts_with("CREATE TABLE IF NOT EXISTS tipwatch.seen"),
            "{sql}"
        );
        assert!(!sql.contains("--"));
        for table in TABLES {
            assert!(
                sql.contains(&format!("CREATE TABLE IF NOT EXISTS {table} ")),
                "{table}"
            );
        }
    }
}
