//! `tidx enrich-l1-fees` — walks pending rows in `kaspa_l2_submissions` /
//! `kaspa_entries` (`WHERE l1_fee_sompi IS NULL`), calls
//! [`FeeResolver::resolve`] for each, and writes the fee back.
//!
//! Compared to `enrich-l1-senders` (which hits api.kaspa.org and is rate-
//! limited to concurrency=2), this runs against LOCAL kaspad + LOCAL PG,
//! so it's sequential-fast — ~30k rows/hour headroom, no external rate
//! limit to nurse.
//!
//! Row outcomes:
//!  - `FeeResolution::Fee(n)` → write `l1_fee_sompi = n`, `l1_fee_enriched_at = now()`.
//!  - `FeeResolution::Coinbase` → leave NULL (Igra L1 carrier txs are
//!    never coinbase; if we ever hit this it's noteworthy, log it).
//!  - `FeeResolution::NotInIndex` → leave NULL, count as "deferred". The
//!    next backfill of `kaspa_tx_index` might fix it; the CLI stays
//!    idempotent so re-runs pick these up when the index grows.
//!  - `Err` from resolver (invariant violation / kaspad error) → warn,
//!    count as failed, move on. Don't halt the whole batch.

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::db;
use crate::kaspa::fee::{BlockFetcher, FeeResolution, FeeResolver, TxLocator};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeeEnrichmentStats {
    pub rows_seen: usize,
    pub fees_written: usize,
    pub coinbase_seen: usize,
    pub deferred_not_in_index: usize,
    pub failed: usize,
}

/// Map a `FeeResolution` to the value we'd write to `l1_fee_sompi`. Only
/// `Fee(n)` produces a write; `Coinbase` and `NotInIndex` return `None`
/// (leave the row NULL). Pure function so the classification is directly
/// unit-testable without a resolver.
pub fn resolution_to_column_value(r: FeeResolution) -> Option<i64> {
    match r {
        FeeResolution::Fee(n) => i64::try_from(n).ok(),
        FeeResolution::Coinbase => None,
        FeeResolution::NotInIndex => None,
    }
}

/// The UPDATE we run for a single enriched row. `table` is interpolated
/// from a fixed CLI whitelist (`kaspa_l2_submissions` or `kaspa_entries`),
/// NOT user-supplied text, so this isn't a SQL-injection surface.
pub fn build_update_sql(table: &str) -> String {
    format!(
        "UPDATE {table}
         SET l1_fee_sompi = $2,
             l1_fee_enriched_at = now()
         WHERE kaspa_txid = $1
           AND l1_fee_sompi IS NULL"
    )
}

/// Walk `WHERE l1_fee_sompi IS NULL` in the given table, resolving each
/// row's fee via the supplied `FeeResolver`. Sequential — see module
/// header for why.
///
/// Cursor semantics: pages by `kaspa_txid > cursor`, not by re-running the
/// same `IS NULL` query. Rows that resolve as `NotInIndex`/`Coinbase` or
/// error stay NULL but their `kaspa_txid` still advances the cursor, so we
/// never re-visit them in the same run — the next CLI invocation (after
/// the tx index has grown, say) gets a fresh cursor and picks them up.
pub async fn enrich_table<L: TxLocator, F: BlockFetcher>(
    pool: &db::Pool,
    resolver: &FeeResolver<'_, L, F>,
    table: &str,
    batch_size: usize,
    max_rows: Option<usize>,
) -> Result<FeeEnrichmentStats> {
    info!(table = %table, "starting L1 fee enrichment");
    let mut stats = FeeEnrichmentStats::default();
    let update_sql = build_update_sql(table);
    let mut cursor: Option<Vec<u8>> = None;

    loop {
        // Pull the next batch after the cursor. Two parameter forms so the
        // first pass ("no cursor yet") doesn't need to bind a sentinel.
        let conn = pool.get().await?;
        let rows = match &cursor {
            None => conn
                .query(
                    &format!(
                        "SELECT kaspa_txid FROM {table}
                         WHERE l1_fee_sompi IS NULL
                         ORDER BY kaspa_txid
                         LIMIT $1"
                    ),
                    &[&(batch_size as i64)],
                )
                .await
                .with_context(|| format!("SELECT pending from {table}"))?,
            Some(after) => conn
                .query(
                    &format!(
                        "SELECT kaspa_txid FROM {table}
                         WHERE l1_fee_sompi IS NULL
                           AND kaspa_txid > $1
                         ORDER BY kaspa_txid
                         LIMIT $2"
                    ),
                    &[&after.as_slice(), &(batch_size as i64)],
                )
                .await
                .with_context(|| format!("SELECT pending after cursor from {table}"))?,
        };
        drop(conn);

        if rows.is_empty() {
            info!(table = %table, "no more rows to enrich");
            break;
        }

        // Advance cursor to the last kaspa_txid we saw in this batch. Every
        // row past this point is either already-non-NULL (filtered) or
        // strictly greater (next batch). Rows we skip in this batch still
        // move the cursor past themselves so we don't loop.
        let last_txid: Vec<u8> = rows.last().unwrap().get(0);
        cursor = Some(last_txid);

        for row in rows {
            if let Some(cap) = max_rows {
                if stats.rows_seen >= cap {
                    info!(
                        table = %table,
                        rows_seen = stats.rows_seen,
                        "max-rows reached"
                    );
                    return Ok(stats);
                }
            }
            stats.rows_seen += 1;
            let txid_vec: Vec<u8> = row.get(0);
            let txid: [u8; 32] = match txid_vec.as_slice().try_into() {
                Ok(a) => a,
                Err(_) => {
                    warn!(
                        len = txid_vec.len(),
                        "row's kaspa_txid is not 32 bytes — skipping"
                    );
                    stats.failed += 1;
                    continue;
                }
            };

            match resolver.resolve(&txid).await {
                Ok(outcome) => {
                    match outcome {
                        FeeResolution::Fee(_) => stats.fees_written += 1,
                        FeeResolution::Coinbase => {
                            warn!(
                                kaspa_txid = %hex::encode(txid),
                                table = %table,
                                "carrier tx resolved as coinbase — unexpected on Igra"
                            );
                            stats.coinbase_seen += 1;
                        }
                        FeeResolution::NotInIndex => {
                            stats.deferred_not_in_index += 1;
                        }
                    }
                    if let Some(fee_i64) = resolution_to_column_value(outcome) {
                        let conn = pool.get().await?;
                        conn.execute(&update_sql, &[&txid.as_slice(), &fee_i64])
                            .await
                            .with_context(|| format!("UPDATE {table} fee"))?;
                    }
                }
                Err(e) => {
                    warn!(
                        kaspa_txid = %hex::encode(txid),
                        err = %e,
                        table = %table,
                        "fee resolve failed; row stays NULL"
                    );
                    stats.failed += 1;
                }
            }
        }

        if let Some(cap) = max_rows {
            if stats.rows_seen >= cap {
                break;
            }
        }
    }

    info!(
        table = %table,
        rows_seen = stats.rows_seen,
        fees_written = stats.fees_written,
        deferred = stats.deferred_not_in_index,
        coinbase = stats.coinbase_seen,
        failed = stats.failed,
        "L1 fee enrichment complete"
    );
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_maps_to_positive_i64() {
        assert_eq!(resolution_to_column_value(FeeResolution::Fee(1000)), Some(1000));
    }

    #[test]
    fn fee_zero_maps_to_zero() {
        // Extremely rare on Kaspa mainnet but arithmetically valid; the
        // enrichment should record it as 0 rather than NULL, otherwise a
        // "fee=0 legitimately" tx is indistinguishable from a "not enriched"
        // one in downstream analytics.
        assert_eq!(resolution_to_column_value(FeeResolution::Fee(0)), Some(0));
    }

    #[test]
    fn fee_at_i64_max_survives() {
        // Ceiling: Kaspa total supply ~28.7 billion KAS = ~2.87e18 sompi,
        // comfortably under i64::MAX (~9.22e18). A single-tx fee wouldn't
        // approach either bound, but we still guard against the cast.
        let big = i64::MAX as u64;
        assert_eq!(
            resolution_to_column_value(FeeResolution::Fee(big)),
            Some(i64::MAX)
        );
    }

    #[test]
    fn fee_overflow_returns_none() {
        // Impossible in practice (see supply cap comment above) but if a
        // resolver ever returned u64 > i64::MAX we prefer NULL over a
        // wraparound value in the DB.
        assert_eq!(
            resolution_to_column_value(FeeResolution::Fee(i64::MAX as u64 + 1)),
            None
        );
    }

    #[test]
    fn coinbase_maps_to_none() {
        assert_eq!(resolution_to_column_value(FeeResolution::Coinbase), None);
    }

    #[test]
    fn not_in_index_maps_to_none() {
        assert_eq!(resolution_to_column_value(FeeResolution::NotInIndex), None);
    }

    #[test]
    fn update_sql_targets_correct_table_and_column() {
        let sql = build_update_sql("kaspa_entries");
        assert!(sql.contains("UPDATE kaspa_entries"));
        assert!(sql.contains("SET l1_fee_sompi"));
        assert!(sql.contains("l1_fee_enriched_at"));
        assert!(
            sql.contains("l1_fee_sompi IS NULL"),
            "guard clause is what keeps re-runs idempotent"
        );
    }

    #[test]
    fn update_sql_supports_both_targets() {
        for t in ["kaspa_entries", "kaspa_l2_submissions"] {
            let sql = build_update_sql(t);
            assert!(sql.starts_with(&format!("UPDATE {t}")));
        }
    }
}
