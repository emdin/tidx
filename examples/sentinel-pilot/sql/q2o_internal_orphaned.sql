-- Q2o: internal transfers a reorg INVALIDATED — the orphaned twin of q2.
-- Same reverted-subtree logic as q2, over orphaned_internal_txs, joined to
-- orphaned_receipts / orphaned_txs / orphaned_blocks on reorg_id (see q1o).
WITH reverted AS (
  SELECT r.tx_hash,
         r.path_idx AS start_p,
         COALESCE((SELECT MIN(n.path_idx) FROM orphaned_internal_txs n
                   WHERE n.reorg_id = r.reorg_id AND n.tx_hash = r.tx_hash
                     AND n.path_idx > r.path_idx AND n.depth <= r.depth), 2147483647) AS end_p
  FROM orphaned_internal_txs r
  WHERE r.reorg_id = {{REORG_ID}}
    AND r.block_num BETWEEN {{BLOCK_LO}} AND {{BLOCK_HI}}
    AND r.error IS NOT NULL
)
SELECT
  38833                         AS chain_id,
  i.reorg_id,
  i.orphaned_at,
  i.block_num,
  b.hash                        AS block_hash,
  i.block_timestamp,
  i.tx_hash,
  i.tx_idx                      AS tx_index,
  'internal'                    AS event_kind,
  NULL::int                     AS log_index,
  i.path_idx                    AS trace_path,
  i."from"                      AS from_address,
  i."to"                        AS to_address,
  CASE WHEN i."from" IN ({{WATCHED}}) THEN i."from" ELSE i."to" END AS matched_watch_address,
  CASE WHEN i."from" IN ({{WATCHED}}) THEN 'out' ELSE 'in' END        AS direction,
  i.value                       AS amount_raw,
  i.call_type,
  i.depth
FROM orphaned_internal_txs i
JOIN orphaned_receipts rc ON rc.reorg_id = i.reorg_id AND rc.tx_hash = i.tx_hash
JOIN orphaned_txs      t  ON t.reorg_id  = i.reorg_id AND t.hash     = i.tx_hash
JOIN orphaned_blocks   b  ON b.reorg_id  = i.reorg_id AND b.num      = i.block_num
WHERE i.reorg_id = {{REORG_ID}}
  AND i.block_num BETWEEN {{BLOCK_LO}} AND {{BLOCK_HI}}
  AND rc.status = 1
  AND i.call_type IN ('CALL', 'CREATE', 'CREATE2', 'SELFDESTRUCT')
  AND i.error IS NULL
  AND i.value::numeric > 0
  AND (i."from" IN ({{WATCHED}}) OR i."to" IN ({{WATCHED}}))
  AND NOT EXISTS (SELECT 1 FROM reverted rv
                  WHERE rv.tx_hash = i.tx_hash AND i.path_idx > rv.start_p AND i.path_idx < rv.end_p)
ORDER BY i.block_num, i.tx_idx, i.path_idx
LIMIT {{PAGE_PLUS_1}}
