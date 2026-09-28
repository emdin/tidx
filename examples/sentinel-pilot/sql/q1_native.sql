-- Q1: top-level native iKAS transfers involving watched addresses.
-- Committed only (receipt status = 1). One row per transaction.
--
-- Placeholders (bot substitutes after validation — /query has no bind params):
--   {{WATCHED}}          comma-separated '0x<40 hex>' literals, e.g. '0xabc…','0xdef…'
--   {{BLOCK_LO}} {{BLOCK_HI}}  inclusive block bounds (exact scan window)
--   {{TS_LO}}            block_timestamp of BLOCK_LO, e.g. '2026-09-28T10:00:00+00:00'
--                        (index-friendly prefilter; the exact bound is block_num)
--   {{CUR_BLOCK}} {{CUR_TX}}  keyset cursor; use -1, -1 for the first page
--   {{PAGE_PLUS_1}}      page_size + 1 — a full page + 1 row means "more"
--
-- Validated on Igra (38833) 2026-09-28. Indexes used: idx_txs_from / idx_txs_to
-- (addr, block_timestamp DESC) — hence TS_LO; block_num is a post-filter.
SELECT
  38833                         AS chain_id,
  t.block_num,
  b.hash                        AS block_hash,
  t.block_timestamp,
  t.hash                        AS tx_hash,
  t.idx                         AS tx_index,
  'native'                      AS event_kind,
  NULL::int                     AS log_index,
  NULL::int                     AS trace_path,
  t."from"                      AS from_address,
  t."to"                        AS to_address,
  CASE WHEN t."from" IN ({{WATCHED}}) THEN t."from" ELSE t."to" END
                                AS matched_watch_address,
  'native'                      AS asset_kind,
  NULL::bytea                   AS token_address,
  t.value                       AS amount_raw,        -- exact integer, wei-scale, as text
  18                            AS decimals,
  t."to"                        AS called_contract,   -- classify via config (exit / router / dex / unknown)
  t.selector,
  r.status
FROM txs t
JOIN receipts r ON r.tx_hash = t.hash
JOIN blocks   b ON b.num = t.block_num
WHERE t.block_num BETWEEN {{BLOCK_LO}} AND {{BLOCK_HI}}
  AND t.block_timestamp >= '{{TS_LO}}'
  AND r.status = 1
  AND t.value::numeric > 0
  AND (t."from" IN ({{WATCHED}}) OR t."to" IN ({{WATCHED}}))
  AND (t.block_num > {{CUR_BLOCK}}
       OR (t.block_num = {{CUR_BLOCK}} AND t.idx > {{CUR_TX}}))
ORDER BY t.block_num, t.idx
LIMIT {{PAGE_PLUS_1}}
