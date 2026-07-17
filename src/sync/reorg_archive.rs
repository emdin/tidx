//! Reorg archive: on reorg, copy displaced rows into `orphaned_*` before the
//! DELETEs, record a `reorgs` event row, all inside one Postgres transaction.
//!
//! Callers own the transaction lifecycle (open + commit/rollback). This lets
//! tests exercise the exact production statement sequence and abort at any
//! point; production `delete_blocks_from` opens+commits its own tx.
//!
//! Column-order safety: INSERT column lists are read from `information_schema`
//! *every reorg* (not cached), so `SELECT t.*` (positional binding) is never
//! used AND a live `ALTER TABLE ... ADD COLUMN` on a source table is picked up
//! on the next reorg without a restart. An unmirrored source column fails the
//! INSERT with a named-column error instead of silently corrupting positionally.
//!
//! Statement construction is six information_schema queries per reorg (one per
//! table pair). Reorgs are rare; the cost is negligible next to the archive
//! INSERTs themselves. The startup [`assert_schema_parity`] check still runs so
//! a drifted migration crashes the boot loop rather than sitting in a state
//! where the next reorg would archive with the wrong columns.

use anyhow::{Result, anyhow};
use deadpool_postgres::Transaction;
use std::collections::HashMap;

/// (source_table, archive_table, block_num_column_on_source).
/// `block_num` for every table except `blocks`, which uses `num`.
const PAIRS: &[(&str, &str, &str)] = &[
    ("logs", "orphaned_logs", "block_num"),
    ("receipts", "orphaned_receipts", "block_num"),
    ("txs", "orphaned_txs", "block_num"),
    ("l2_withdrawals", "orphaned_l2_withdrawals", "block_num"),
    ("internal_txs", "orphaned_internal_txs", "block_num"),
    ("blocks", "orphaned_blocks", "num"),
];

/// One archive INSERT + its matching DELETE, for a single table pair, built
/// from live information_schema at reorg time.
struct ArchiveStmt {
    insert_sql: String,
    delete_sql: String,
}

/// Read the source table's real column layout from `information_schema` and
/// produce the archive INSERT + source DELETE for it.
///
/// Column filter: skip `GENERATED` and `IDENTITY` columns — those aren't
/// insertable directly and would fail the INSERT if a future migration adds
/// one. `current_schema()` scopes the read so a duplicate table name in
/// another schema (e.g. an ops-analytics schema) doesn't cross-contaminate.
async fn build_stmts_for(
    tx: &Transaction<'_>,
    source: &str,
    archive: &str,
    block_col: &str,
) -> Result<ArchiveStmt> {
    let rows = tx
        .query(
            r#"SELECT column_name
                 FROM information_schema.columns
                WHERE table_name = $1
                  AND table_schema = current_schema()
                  AND is_generated = 'NEVER'
                  AND is_identity = 'NO'
                ORDER BY ordinal_position"#,
            &[&source],
        )
        .await?;
    if rows.is_empty() {
        return Err(anyhow!(
            "source table {source} not found in current_schema()"
        ));
    }
    let cols: Vec<String> = rows.iter().map(|r| r.get::<_, String>(0)).collect();
    // Quote every column — some are reserved words (`to`, `from`).
    let quoted: Vec<String> = cols.iter().map(|c| format!("\"{c}\"")).collect();
    let cols_list = quoted.join(", ");
    let insert_sql = format!(
        "INSERT INTO {archive} (reorg_id, {cols_list}) \
         SELECT $1, {cols_list} FROM {source} WHERE {block_col} >= $2"
    );
    let delete_sql = format!("DELETE FROM {source} WHERE {block_col} >= $1");
    Ok(ArchiveStmt {
        insert_sql,
        delete_sql,
    })
}

/// Result of one reorg mutation: `reorg_id` written, and per-table row counts.
/// All counts stay i64 through the API even though `reorgs.*_removed` are
/// stored as INT4 — the code guards the downcast at the write site.
pub struct ReorgResult {
    pub reorg_id: i64,
    pub fork_point: i64,
    pub prev_tip: i64,
    pub depth: i32,
    pub blocks_removed: i64,
    pub txs_removed: i64,
    pub logs_removed: i64,
    pub receipts_removed: i64,
    pub internal_txs_removed: i64,
    pub withdrawals_removed: i64,
}

/// Full reorg mutation on an externally-owned transaction. Order:
///   1. `SELECT max(num) FROM blocks` → prev_tip (inside the tx).
///   2. Guard: if depth == 0, return Ok early (no reorgs event row written).
///   3. Build six per-pair archive stmts from live information_schema.
///   4. INSERT into reorgs, RETURNING id.
///   5. Six archive INSERTs (each source row copied to its orphaned_* twin).
///   6. Six DELETEs on the source tables.
///   7. UPDATE reorgs SET counts, count_check_ok.
///   8. (Caller: rewind sync_state.tip_num inside the same tx if desired,
///      then commit.)
///
/// `fork_point` is the last common block (kept in canonical); rows with
/// `block_num >= fork_point + 1` (or `num >= fork_point + 1` on blocks) are
/// displaced.
pub async fn apply_reorg_mutation<'a>(
    tx: &Transaction<'a>,
    fork_point: i64,
) -> Result<ReorgResult> {
    // 1. prev_tip inside the tx (batches may have advanced past mismatch - 1).
    let prev_tip: i64 = tx
        .query_one("SELECT COALESCE(max(num), 0)::bigint FROM blocks", &[])
        .await?
        .get(0);
    let depth_i64: i64 = (prev_tip - fork_point).max(0);
    let cut: i64 = fork_point + 1;

    // 2. depth==0 guard: nothing to displace; skip the event row entirely so
    //    retried/spurious reorg calls don't pollute the audit log.
    if depth_i64 == 0 {
        return Ok(ReorgResult {
            reorg_id: 0,
            fork_point,
            prev_tip,
            depth: 0,
            blocks_removed: 0,
            txs_removed: 0,
            logs_removed: 0,
            receipts_removed: 0,
            internal_txs_removed: 0,
            withdrawals_removed: 0,
        });
    }
    let depth: i32 = depth_i64.try_into().map_err(|_| {
        anyhow!("reorg depth {depth_i64} exceeds i32::MAX (schema stores reorgs.depth as INT4)")
    })?;

    // 3. Build six per-pair statements from *current* information_schema. NOT
    //    cached — a hot ALTER TABLE ... ADD COLUMN on a source table (mirrored
    //    on its orphaned_* twin) is picked up on the next reorg without a
    //    restart. See module docstring for the rationale.
    let mut stmts: HashMap<&str, ArchiveStmt> = HashMap::with_capacity(PAIRS.len());
    for (src, arc, blk) in PAIRS {
        stmts.insert(*src, build_stmts_for(tx, src, arc, blk).await?);
    }

    // 4. reorgs row (counts zeroed; step 7 fills them).
    let reorg_id: i64 = tx
        .query_one(
            r#"INSERT INTO reorgs
                 (fork_point, prev_tip, depth,
                  blocks_removed, txs_removed, logs_removed,
                  receipts_removed, internal_txs_removed, withdrawals_removed)
               VALUES ($1, $2, $3, 0, 0, 0, 0, 0, 0)
               RETURNING id"#,
            &[&fork_point, &prev_tip, &depth],
        )
        .await?
        .get(0);

    // 5. archive INSERTs, ordered so any future source→source FKs (none today)
    //    would be honored on both the archive and the delete side.
    let mut counts: HashMap<&str, i64> = HashMap::with_capacity(PAIRS.len());
    for (src, _arc, _blk) in PAIRS {
        let stmt = stmts.get(src).expect("stmt was inserted just above");
        let n = tx.execute(&stmt.insert_sql, &[&reorg_id, &cut]).await? as i64;
        counts.insert(*src, n);
    }

    // 6. DELETEs, same source order.
    for (src, _, _) in PAIRS {
        let stmt = stmts.get(src).expect("stmt was inserted just above");
        tx.execute(&stmt.delete_sql, &[&cut]).await?;
    }

    // 7. counts + count_check_ok. blocks_removed should equal depth when the
    //    canonical head is contiguous. Anomaly is logged AND recorded queryable
    //    via reorgs.count_check_ok; the downcast panics on overflow but is
    //    bounded by MAX_REORG_DEPTH (128) × per-block row counts in production.
    fn to_i32(name: &str, n: i64) -> Result<i32> {
        i32::try_from(n).map_err(|_| {
            anyhow!("reorg {name} count {n} exceeds i32::MAX (schema stores it as INT4)")
        })
    }
    let blocks_removed = *counts.get("blocks").unwrap_or(&0);
    let txs_removed = *counts.get("txs").unwrap_or(&0);
    let logs_removed = *counts.get("logs").unwrap_or(&0);
    let receipts_removed = *counts.get("receipts").unwrap_or(&0);
    let internal_txs_removed = *counts.get("internal_txs").unwrap_or(&0);
    let withdrawals_removed = *counts.get("l2_withdrawals").unwrap_or(&0);
    let count_check_ok = blocks_removed == depth_i64;
    if !count_check_ok {
        tracing::warn!(
            fork_point,
            prev_tip,
            depth,
            blocks_removed,
            "reorg count mismatch: blocks_removed != depth (non-contiguous head?)"
        );
    }
    tx.execute(
        r#"UPDATE reorgs
             SET blocks_removed        = $1,
                 txs_removed           = $2,
                 logs_removed          = $3,
                 receipts_removed      = $4,
                 internal_txs_removed  = $5,
                 withdrawals_removed   = $6,
                 count_check_ok        = $7
           WHERE id = $8"#,
        &[
            &to_i32("blocks_removed", blocks_removed)?,
            &to_i32("txs_removed", txs_removed)?,
            &to_i32("logs_removed", logs_removed)?,
            &to_i32("receipts_removed", receipts_removed)?,
            &to_i32("internal_txs_removed", internal_txs_removed)?,
            &to_i32("withdrawals_removed", withdrawals_removed)?,
            &count_check_ok,
            &reorg_id,
        ],
    )
    .await?;

    Ok(ReorgResult {
        reorg_id,
        fork_point,
        prev_tip,
        depth,
        blocks_removed,
        txs_removed,
        logs_removed,
        receipts_removed,
        internal_txs_removed,
        withdrawals_removed,
    })
}

/// Startup schema-parity check. Panics on drift with a message naming the
/// offending pair; leaves the process free to crash-loop rather than sit in a
/// state where the next reorg would corrupt.
///
/// The check is the same DO $$ block as db/reorg_archive.sql's design intent:
/// ordered, typed column arrays must equal `[reorg_id, orphaned_at] || <source>`.
/// Filters by `current_schema()` — matches [`build_stmts_for`] so a duplicate
/// table name in a sibling schema (e.g. an ops-analytics scratch schema)
/// doesn't cause a bogus mismatch at boot.
pub async fn assert_schema_parity(pool: &crate::db::Pool) -> Result<()> {
    let conn = pool.get().await?;
    conn.batch_execute(
        r#"
        DO $$
        DECLARE
          pairs text[][] := ARRAY[
            ARRAY['txs','orphaned_txs'],
            ARRAY['logs','orphaned_logs'],
            ARRAY['receipts','orphaned_receipts'],
            ARRAY['internal_txs','orphaned_internal_txs'],
            ARRAY['l2_withdrawals','orphaned_l2_withdrawals'],
            ARRAY['blocks','orphaned_blocks']
          ];
          src text; arc text;
          src_arr text[]; arc_arr text[]; want text[];
        BEGIN
          FOR i IN 1..array_length(pairs, 1) LOOP
            src := pairs[i][1]; arc := pairs[i][2];
            SELECT array_agg(column_name || ':' || data_type ORDER BY ordinal_position)
              INTO src_arr FROM information_schema.columns
             WHERE table_name = src AND table_schema = current_schema();
            SELECT array_agg(column_name || ':' || data_type ORDER BY ordinal_position)
              INTO arc_arr FROM information_schema.columns
             WHERE table_name = arc AND table_schema = current_schema();
            want := ARRAY['reorg_id:bigint','orphaned_at:timestamp with time zone'] || src_arr;
            IF arc_arr <> want THEN
              RAISE EXCEPTION
                'reorg archive schema parity mismatch: % vs % — mirrored migration missing?', src, arc;
            END IF;
          END LOOP;
        END $$;
        "#,
    )
    .await?;
    Ok(())
}
