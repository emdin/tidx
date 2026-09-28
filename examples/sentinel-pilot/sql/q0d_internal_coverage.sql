-- Q0d: INTERIM internal-trace coverage signal for a window. Report, never assume.
--
-- tidx records no per-transaction trace outcome (verified 2026-09-28):
--   * the realtime tracer logs "Failed to trace tx; skipping" and moves on —
--     no retry, no marker (src/sync/trace.rs);
--   * the gap-fill writer (sync_range_standalone) fetches blocks + receipts and
--     never traces at all;
--   * a trace that succeeds with zero nested frames writes nothing
--     (write_internal_txs returns early on empty), so "no rows" is
--     indistinguishable from "never traced";
--   * `tidx backfill-traces` re-traces every tx lacking rows, blindly.
-- So an empty q2 can mean missing evidence. Until a trace-coverage record
-- exists in tidx (proposed as a separate change), the bot must report
-- `internal_coverage: unresolved` for any window where this query's
-- `possibly_untraced` is > 0, and may re-run `tidx backfill-traces --from
-- --to` for that range.
--
-- Heuristic, honest about its limits: a receipt with gas_used = 21000 is a
-- plain value transfer that executed no code and CANNOT have nested frames, so
-- those are certainly covered. Every other successful tx executed code; those
-- with no internal_txs rows are EITHER leaf calls that made no nested call
-- (common — e.g. an ERC-20 transfer() emits a log but calls nothing) OR
-- untraced. This query cannot tell them apart; it bounds the uncertainty.
SELECT
  count(*)                                                     AS successful_txs,
  count(*) FILTER (WHERE r.gas_used = 21000)                   AS plain_transfers_certainly_covered,
  count(*) FILTER (WHERE r.gas_used > 21000
                     AND EXISTS (SELECT 1 FROM internal_txs i WHERE i.tx_hash = t.hash))
                                                               AS traced_with_frames,
  count(*) FILTER (WHERE r.gas_used > 21000
                     AND NOT EXISTS (SELECT 1 FROM internal_txs i WHERE i.tx_hash = t.hash))
                                                               AS possibly_untraced
FROM txs t
JOIN receipts r ON r.tx_hash = t.hash
WHERE t.block_num BETWEEN {{BLOCK_LO}} AND {{BLOCK_HI}}
  AND t.block_timestamp >= '{{TS_LO}}'
  AND r.status = 1
