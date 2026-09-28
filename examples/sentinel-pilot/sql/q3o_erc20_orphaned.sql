-- Q3o: ERC-20 Transfer events a reorg INVALIDATED — the orphaned twin of q3.
-- Joins orphaned_logs ⋈ orphaned_txs ⋈ orphaned_blocks on reorg_id so the row
-- is the pre-reorg observation with its original block hash (see q1o).
-- Logs of orphaned txs are themselves orphaned; no receipt join is needed
-- (logs only exist for successful txs).
SELECT
  38833                         AS chain_id,
  l.reorg_id,
  l.orphaned_at,
  l.block_num,
  b.hash                        AS block_hash,
  l.block_timestamp,
  l.tx_hash,
  l.tx_idx                      AS tx_index,
  'erc20'                       AS event_kind,
  l.log_idx                     AS log_index,
  NULL::int                     AS trace_path,
  abi_address(l.topic1)         AS from_address,
  abi_address(l.topic2)         AS to_address,
  CASE WHEN l.topic1 IN ({{WATCHED_TOPICS}}) THEN abi_address(l.topic1) ELSE abi_address(l.topic2) END AS matched_watch_address,
  CASE WHEN l.topic1 IN ({{WATCHED_TOPICS}}) THEN 'out' ELSE 'in' END AS direction,
  l.address                     AS token_address,
  format_uint(l.data)           AS amount_raw,
  t."to"                        AS called_contract,
  t.selector
FROM orphaned_logs l
JOIN orphaned_txs    t ON t.reorg_id = l.reorg_id AND t.hash = l.tx_hash
JOIN orphaned_blocks b ON b.reorg_id = l.reorg_id AND b.num = l.block_num
WHERE l.reorg_id = {{REORG_ID}}
  AND l.block_num BETWEEN {{BLOCK_LO}} AND {{BLOCK_HI}}
  AND l.topic0 = '0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef'
  AND l.topic3 IS NULL
  AND abi_uint(l.data) > 0
  AND (l.topic1 IN ({{WATCHED_TOPICS}}) OR l.topic2 IN ({{WATCHED_TOPICS}}))
  AND (l.block_num > {{CUR_BLOCK}}
       OR (l.block_num = {{CUR_BLOCK}} AND l.tx_idx > {{CUR_TX}})
       OR (l.block_num = {{CUR_BLOCK}} AND l.tx_idx = {{CUR_TX}} AND l.log_idx > {{CUR_LOG}}))
ORDER BY l.block_num, l.tx_idx, l.log_idx
LIMIT {{PAGE_PLUS_1}}
