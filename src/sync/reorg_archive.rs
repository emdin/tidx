//! Reorg archive: on reorg, copy displaced rows into `orphaned_*` before the
//! DELETEs, record a `reorgs` event row, all inside one Postgres transaction.
//!
//! Callers own the transaction lifecycle (open + commit/rollback). This lets
//! tests exercise the exact production statement sequence and abort at any
//! point; production `delete_blocks_from` opens+commits its own tx.
//!
//! Column-order safety: INSERT column lists are read from `information_schema`
//! once at first call and cached, so `SELECT t.*` (positional binding) is
//! never used. An unmirrored source column fails the INSERT with a named-column
//! error instead of silently corrupting positionally.

use anyhow::{Result, anyhow};
use deadpool_postgres::Transaction;
use std::collections::HashMap;
use tokio::sync::OnceCell;

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

/// One prepared archive INSERT + its matching DELETE, for a single table pair.
struct ArchiveStmt {
    /// e.g. `INSERT INTO orphaned_txs (reorg_id, block_num, ..., signature_type)
    ///        SELECT $1, block_num, ..., signature_type FROM txs WHERE block_num >= $2`
    insert_sql: String,
    /// e.g. `DELETE FROM txs WHERE block_num >= $1`
    delete_sql: String,
}

static STMTS: OnceCell<HashMap<&'static str, ArchiveStmt>> = OnceCell::const_new();

/// Build the archive INSERT + DELETE statement pair for one source table by
/// reading `information_schema.columns` (source cols only — provenance columns
/// on the archive are supplied by the INSERT itself).
async fn build_stmts_for(
    conn: &deadpool_postgres::Object,
    source: &str,
    archive: &str,
    block_col: &str,
) -> Result<ArchiveStmt> {
    let rows = conn
        .query(
            r#"SELECT column_name
                 FROM information_schema.columns
                WHERE table_name = $1 AND table_schema = current_schema()
                ORDER BY ordinal_position"#,
            &[&source],
        )
        .await?;
    if rows.is_empty() {
        return Err(anyhow!("source table {source} not found in information_schema"));
    }
    let cols: Vec<String> = rows.iter().map(|r| r.get::<_, String>(0)).collect();
    // Quote every column — some are reserved words (`to`, `from`) or contain them.
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

async fn init_stmts(pool: &crate::db::Pool) -> Result<HashMap<&'static str, ArchiveStmt>> {
    let conn = pool.get().await?;
    let mut out = HashMap::new();
    for (src, arc, blk) in PAIRS {
        let stmt = build_stmts_for(&conn, src, arc, blk).await?;
        out.insert(*src, stmt);
    }
    Ok(out)
}

/// Ensure the statement cache is populated. Idempotent; safe to call on every
/// reorg. First call reads `information_schema` (six small queries); later
/// calls are a `OnceCell` fast-path.
pub async fn ensure_initialized(pool: &crate::db::Pool) -> Result<()> {
    STMTS
        .get_or_try_init(|| async { init_stmts(pool).await })
        .await?;
    Ok(())
}

/// Result of one reorg mutation: `reorg_id` written, and per-table row counts.
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
///   2. INSERT into reorgs, RETURNING id.
///   3. Six archive INSERTs (each source row copied to its orphaned_* twin).
///   4. Six DELETEs on the source tables.
///   5. UPDATE reorgs SET counts, count_check_ok.
///   6. (Caller: rewind sync_state.tip_num inside the same tx if desired,
///      then commit.)
///
/// `fork_point` is the last common block (kept in canonical); rows with
/// `block_num >= fork_point + 1` (or `num >= fork_point + 1` on blocks) are
/// displaced.
pub async fn apply_reorg_mutation<'a>(
    tx: &Transaction<'a>,
    fork_point: i64,
) -> Result<ReorgResult> {
    let stmts = STMTS
        .get()
        .ok_or_else(|| anyhow!("reorg_archive::ensure_initialized was not called"))?;

    // 1. prev_tip inside the tx (batches may have advanced past mismatch - 1).
    let prev_tip: i64 = tx
        .query_one("SELECT COALESCE(max(num), 0)::bigint FROM blocks", &[])
        .await?
        .get(0);
    let depth: i32 = (prev_tip - fork_point).max(0) as i32;
    let cut: i64 = fork_point + 1;

    // 2. reorgs row (counts zeroed; step 5 fills them).
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

    // 3. archive INSERTs, in the same order the deletes will run (FK-safe
    //    since orphaned_* has FK only to reorgs, which now exists).
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for (src, _arc, _blk) in PAIRS {
        let stmt = stmts.get(src).ok_or_else(|| anyhow!("no stmt for {src}"))?;
        let n = tx.execute(&stmt.insert_sql, &[&reorg_id, &cut]).await? as i64;
        counts.insert(*src, n);
    }

    // 4. DELETEs, canonical, source order matches archive order above.
    for (src, _, _) in PAIRS {
        let stmt = stmts.get(src).ok_or_else(|| anyhow!("no stmt for {src}"))?;
        tx.execute(&stmt.delete_sql, &[&cut]).await?;
    }

    // 5. counts + count_check_ok. blocks_removed should equal depth when the
    //    canonical head is contiguous (it always is in production). Anomaly is
    //    logged AND recorded queryable — see reorgs.count_check_ok.
    let blocks_removed = *counts.get("blocks").unwrap_or(&0);
    let count_check_ok = blocks_removed == depth as i64;
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
            &(blocks_removed as i32),
            &(counts["txs"] as i32),
            &(counts["logs"] as i32),
            &(counts["receipts"] as i32),
            &(counts["internal_txs"] as i32),
            &(counts["l2_withdrawals"] as i32),
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
        txs_removed: counts["txs"],
        logs_removed: counts["logs"],
        receipts_removed: counts["receipts"],
        internal_txs_removed: counts["internal_txs"],
        withdrawals_removed: counts["l2_withdrawals"],
    })
}

/// Startup schema-parity check. Panics on drift with a message naming the
/// offending pair; leaves the process free to crash-loop rather than sit in a
/// state where the next reorg would corrupt.
///
/// The check is the same DO $$ block as db/reorg_archive.sql's design intent:
/// ordered, typed column arrays must equal `[reorg_id, orphaned_at] || <source>`.
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
              INTO src_arr FROM information_schema.columns WHERE table_name = src;
            SELECT array_agg(column_name || ':' || data_type ORDER BY ordinal_position)
              INTO arc_arr FROM information_schema.columns WHERE table_name = arc;
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
