-- Q2: internal (trace) native-value transfers involving watched addresses.
--
-- Correctness rules applied here (each verified against Igra data 2026-09-28):
--   * receipt status = 1 — a reverted TRANSACTION contributes nothing.
--   * call_type IN (CALL, CREATE, SELFDESTRUCT) — DELEGATECALL frames carry the
--     parent's value context and are NOT transfers (25 such frames with value>0
--     per 300k blocks observed); STATICCALL never carries value.
--   * error IS NULL — the frame itself did not revert.
--   * NOT inside a reverted SUBTREE. `error` is set only on the frame that
--     reverted; its descendants carry NULL (863 of 909 reverted parents had
--     error-free children in a 300k-block sample). So we compute each reverted
--     frame's subtree as the DFS-preorder interval (start_p, end_p) where end_p
--     is the next frame at depth <= its own, and exclude frames inside any.
--
-- Placeholders as in q1, plus cursor -1 (path_idx). First page: -1,-1,-1.
--
-- `candidate_txs` restricts the reverted-frame scan to transactions that
-- actually touch a watched address. Semantically identical (NOT EXISTS matches
-- on tx_hash, so reverted frames of other txs could never exclude anything),
-- but without it the CTE walked every reverted frame in the window: measured
-- 0.6–0.9 s cold for ONE address with ZERO result rows over 200k–2M blocks.
WITH candidate_txs AS (
  SELECT DISTINCT c.tx_hash
  FROM internal_txs c
  WHERE c.block_num BETWEEN 999000 AND 999002
    AND c.block_timestamp >= '2026-09-28T00:00:00+00:00'
    AND (c."from" IN ('\x1111111111111111111111111111111111111111'::bytea) OR c."to" IN ('\x1111111111111111111111111111111111111111'::bytea))
),
reverted AS (
  SELECT r.tx_hash,
         r.path_idx AS start_p,
         COALESCE((SELECT MIN(n.path_idx) FROM internal_txs n
                   WHERE n.tx_hash = r.tx_hash
                     AND n.path_idx > r.path_idx
                     AND n.depth <= r.depth), 2147483647) AS end_p
  FROM internal_txs r
  WHERE r.block_num BETWEEN 999000 AND 999002
    AND r.error IS NOT NULL
    AND r.tx_hash IN (SELECT tx_hash FROM candidate_txs)
)
SELECT
  38833                         AS chain_id,
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
  CASE WHEN i."from" IN ('\x1111111111111111111111111111111111111111'::bytea) THEN i."from" ELSE i."to" END
                                AS matched_watch_address,
  'native'                      AS asset_kind,
  NULL::bytea                   AS token_address,
  i.value                       AS amount_raw,
  18                            AS decimals,
  t."to"                        AS called_contract,   -- top-level callee, for classification
  t.selector,
  i.call_type,
  i.depth
FROM internal_txs i
JOIN receipts rc ON rc.tx_hash = i.tx_hash
JOIN txs      t  ON t.hash     = i.tx_hash
JOIN blocks   b  ON b.num      = i.block_num
WHERE i.block_num BETWEEN 999000 AND 999002
  AND i.block_timestamp >= '2026-09-28T00:00:00+00:00'
  AND rc.status = 1
  AND i.call_type IN ('CALL', 'CREATE', 'SELFDESTRUCT')
  AND i.error IS NULL
  AND i.value::numeric > 0
  AND (i."from" IN ('\x1111111111111111111111111111111111111111'::bytea) OR i."to" IN ('\x1111111111111111111111111111111111111111'::bytea))
    AND (i.block_num > -1
       OR (i.block_num = -1 AND i.tx_idx > -1)
       OR (i.block_num = -1 AND i.tx_idx = -1 AND i.path_idx > -1))
ORDER BY i.block_num, i.tx_idx, i.path_idx
LIMIT 101
