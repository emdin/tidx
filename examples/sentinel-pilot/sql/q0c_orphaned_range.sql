-- Q0c: blocks orphaned by a reorg in a range (for issuing corrections).
-- Every orphaned_* table mirrors its source table's columns exactly (tidx
-- asserts schema parity at boot), so q1/q2/q3 can be re-pointed at
-- orphaned_txs / orphaned_internal_txs / orphaned_logs verbatim to enumerate
-- the observations that are now invalid. reorgs(id, fork_point, prev_tip,
-- depth, ...) has the event itself. Igra has recorded 0 reorgs to date.
SELECT reorg_id, num, hash, orphaned_at
FROM orphaned_blocks
WHERE num BETWEEN {{BLOCK_LO}} AND {{BLOCK_HI}}
ORDER BY num
