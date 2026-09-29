SET max_parallel_workers_per_gather = 0;
-- Q0e: trace coverage of a window from tidx's per-tx trace record
-- (`trace_outcomes`, added in this PR). Replaces the q0d heuristic once the
-- table is deployed; until then q0d is the only signal.
--
--   ok        traced, frames written to internal_txs
--   empty     traced, no nested call — a real "nothing happened", not a gap
--   failed    every attempt errored; the engine repair loop / backfill-traces retry it
--   untraced  no record at all (indexed before the table existed, or with tracing off)
--
-- Bot rule: internal coverage for the window is RESOLVED only when
-- failed = 0 AND untraced = 0. Otherwise persist the window in
-- unresolved_internal_ranges and replay q2 after `tidx backfill-traces
-- --from --to` (which also stamps `ok` on already-traced history with
-- --mark-existing, no RPC needed).
SELECT
  count(*)                                                       AS successful_txs,
  count(*) FILTER (WHERE o.outcome = 'ok')                       AS ok,
  count(*) FILTER (WHERE o.outcome = 'empty')                    AS empty,
  count(*) FILTER (WHERE o.outcome = 'failed')                   AS failed,
  count(*) FILTER (WHERE o.tx_hash IS NULL)                      AS untraced,
  (count(*) FILTER (WHERE o.outcome = 'failed' OR o.tx_hash IS NULL)) = 0
                                                                 AS resolved
FROM txs t
JOIN receipts r ON r.tx_hash = t.hash
LEFT JOIN trace_outcomes o ON o.tx_hash = t.hash
WHERE t.block_num BETWEEN 0 AND 17976809
  AND t.block_timestamp >= '2026-01-01T00:00:00+00:00'
  AND r.status = 1
;
