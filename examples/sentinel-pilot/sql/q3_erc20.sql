-- Q3: ERC-20 Transfer events (incl. WiKAS) involving watched addresses.
--
-- * topic0 = Transfer(address,address,uint256).
-- * topic3 IS NULL excludes ERC-721, which shares topic0 but puts tokenId in
--   topic3 (verified: 273,407 ERC-20-shaped vs 6,656 NFT-shaped on Igra).
-- * No receipts join: logs exist only for successful transactions (verified:
--   0 logs belong to status=0 txs across 1,124 reverted txs in a 300k window).
-- * decimals is NULL here — tidx has no token registry on Igra. The bot keeps a
--   token->decimals map (WiKAS 0x17ec…c242 = 18, read via eth_call decimals()).
--
-- {{WATCHED_TOPICS}}: the watched addresses as 32-byte topics, i.e.
--   '0x000000000000000000000000<40 hex>' literals (left-pad 24 zeros, lowercase).
-- Cursor {{CUR_LOG}} = log_idx. First page: -1,-1,-1.
SELECT
  38833                         AS chain_id,
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
  CASE WHEN l.topic1 IN ({{WATCHED_TOPICS}}) THEN abi_address(l.topic1) ELSE abi_address(l.topic2) END
                                AS matched_watch_address,
  'erc20'                       AS asset_kind,
  l.address                     AS token_address,
  format_uint(l.data)           AS amount_raw,        -- text; survives uint256 (abi_uint nulls >= 2^96)
  NULL::int                     AS decimals,
  t."to"                        AS called_contract,
  t.selector
FROM logs l
JOIN txs    t ON t.hash = l.tx_hash
JOIN blocks b ON b.num  = l.block_num
WHERE l.block_num BETWEEN {{BLOCK_LO}} AND {{BLOCK_HI}}
  AND l.block_timestamp >= '{{TS_LO}}'
  AND l.topic0 = '0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef'
  AND l.topic3 IS NULL
  AND (l.topic1 IN ({{WATCHED_TOPICS}}) OR l.topic2 IN ({{WATCHED_TOPICS}}))
  AND (l.block_num > {{CUR_BLOCK}}
       OR (l.block_num = {{CUR_BLOCK}} AND l.tx_idx > {{CUR_TX}})
       OR (l.block_num = {{CUR_BLOCK}} AND l.tx_idx = {{CUR_TX}} AND l.log_idx > {{CUR_LOG}}))
ORDER BY l.block_num, l.tx_idx, l.log_idx
LIMIT {{PAGE_PLUS_1}}
