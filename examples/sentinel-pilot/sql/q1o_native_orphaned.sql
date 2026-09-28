-- Q1o: native transfers that a reorg INVALIDATED — the orphaned twin of q1.
--
-- Do not simply re-point q1 at orphaned_txs: q1 joins receipts and blocks,
-- which are CANONICAL tables, so an orphaned tx would either vanish (no
-- canonical receipt) or pick up the REPLACEMENT block's hash. This query joins
-- orphaned_txs ⋈ orphaned_receipts ⋈ orphaned_blocks on the same reorg_id, so
-- every row is the observation exactly as it was before the reorg, with its
-- original block hash. Emit a CORRECTION for each alert whose dedupe key
-- matches a row here; then rescan the canonical range above fork_point.
--
-- {{REORG_ID}} from `SELECT id, fork_point, prev_tip, depth, occurred_at FROM reorgs`.
-- Paginated exactly like q1: keyset (block_num, tx_index), cursor {{CUR_BLOCK}},{{CUR_TX}};
-- first page -1,-1. A reorg can orphan more than a page of movements in ONE block.
SELECT
  38833                         AS chain_id,
  t.reorg_id,
  t.orphaned_at,
  t.block_num,
  b.hash                        AS block_hash,        -- the ORPHANED block's hash
  t.block_timestamp,
  t.hash                        AS tx_hash,
  t.idx                         AS tx_index,
  'native'                      AS event_kind,
  NULL::int                     AS log_index,
  NULL::int                     AS trace_path,
  t."from"                      AS from_address,
  t."to"                        AS to_address,
  CASE WHEN t."from" IN ({{WATCHED}}) THEN t."from" ELSE t."to" END AS matched_watch_address,
  CASE WHEN t."from" IN ({{WATCHED}}) THEN 'out' ELSE 'in' END        AS direction,
  t.value                       AS amount_raw,
  r.status
FROM orphaned_txs t
JOIN orphaned_receipts r ON r.reorg_id = t.reorg_id AND r.tx_hash = t.hash
JOIN orphaned_blocks   b ON b.reorg_id = t.reorg_id AND b.num = t.block_num
WHERE t.reorg_id = {{REORG_ID}}
  AND t.block_num BETWEEN {{BLOCK_LO}} AND {{BLOCK_HI}}
  AND r.status = 1
  AND t.value::numeric > 0
  AND (t."from" IN ({{WATCHED}}) OR t."to" IN ({{WATCHED}}))
  AND (t.block_num > {{CUR_BLOCK}}
       OR (t.block_num = {{CUR_BLOCK}} AND t.idx > {{CUR_TX}}))
ORDER BY t.block_num, t.idx
LIMIT {{PAGE_PLUS_1}}
