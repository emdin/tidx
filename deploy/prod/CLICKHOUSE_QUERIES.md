# ClickHouse Query Pack

Base endpoint:

```bash
https://explorer.igralabs.com/query
```

Notes:

- In ClickHouse here, hashes and addresses are stored as `0x...` strings.
- For public use, keep result sizes bounded with `LIMIT`.
- Replace `0xYOUR_ADDRESS` or `0xYOUR_CONTRACT` where needed.
- The curl helper below is the tested way to send multiline SQL safely.

## Curl Helper

Load this once in your shell:

```bash
run_ch_query() {
  local sql
  sql="$(cat)"
  curl --get 'https://explorer.igralabs.com/query' \
    --data-urlencode 'chainId=38833' \
    --data-urlencode 'engine=clickhouse' \
    --data-urlencode "sql=$sql"
}
```

## Top 20 Useful Queries

### 1. Latest 20 blocks

```sql
SELECT num, hash, timestamp, gas_used
FROM blocks
ORDER BY num DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT num, hash, timestamp, gas_used
FROM blocks
ORDER BY num DESC
LIMIT 20
SQL
```

### 2. Latest 20 transactions

```sql
SELECT block_num, idx, hash, `from`, `to`, gas_used
FROM txs
ORDER BY block_num DESC, idx DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT block_num, idx, hash, `from`, `to`, gas_used
FROM txs
ORDER BY block_num DESC, idx DESC
LIMIT 20
SQL
```

### 3. Latest 20 receipts

```sql
SELECT block_num, tx_idx, tx_hash, status, gas_used
FROM receipts
ORDER BY block_num DESC, tx_idx DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT block_num, tx_idx, tx_hash, status, gas_used
FROM receipts
ORDER BY block_num DESC, tx_idx DESC
LIMIT 20
SQL
```

### 4. Latest 20 logs

```sql
SELECT block_num, log_idx, tx_hash, address, topic0
FROM logs
ORDER BY block_num DESC, log_idx DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT block_num, log_idx, tx_hash, address, topic0
FROM logs
ORDER BY block_num DESC, log_idx DESC
LIMIT 20
SQL
```

### 5. Basic chain totals

```sql
SELECT
  (SELECT count() FROM blocks) AS blocks,
  (SELECT count() FROM txs) AS txs,
  (SELECT count() FROM receipts) AS receipts,
  (SELECT count() FROM logs) AS logs
```

```bash
cat <<'SQL' | run_ch_query
SELECT
  (SELECT count() FROM blocks) AS blocks,
  (SELECT count() FROM txs) AS txs,
  (SELECT count() FROM receipts) AS receipts,
  (SELECT count() FROM logs) AS logs
SQL
```

### 6. Daily transactions

```sql
SELECT toDate(block_timestamp) AS day, count() AS txs
FROM txs
GROUP BY day
ORDER BY day DESC
LIMIT 30
```

```bash
cat <<'SQL' | run_ch_query
SELECT toDate(block_timestamp) AS day, count() AS txs
FROM txs
GROUP BY day
ORDER BY day DESC
LIMIT 30
SQL
```

### 7. Daily successful vs failed transactions

```sql
SELECT
  toDate(block_timestamp) AS day,
  countIf(status = 1) AS success_txs,
  countIf(status = 0) AS failed_txs
FROM receipts
GROUP BY day
ORDER BY day DESC
LIMIT 30
```

```bash
cat <<'SQL' | run_ch_query
SELECT
  toDate(block_timestamp) AS day,
  countIf(status = 1) AS success_txs,
  countIf(status = 0) AS failed_txs
FROM receipts
GROUP BY day
ORDER BY day DESC
LIMIT 30
SQL
```

### 8. Hourly TPS approximation

```sql
SELECT
  toStartOfHour(block_timestamp) AS hour,
  count() AS txs,
  round(count() / 3600, 4) AS avg_tps
FROM txs
GROUP BY hour
ORDER BY hour DESC
LIMIT 48
```

```bash
cat <<'SQL' | run_ch_query
SELECT
  toStartOfHour(block_timestamp) AS hour,
  count() AS txs,
  round(count() / 3600, 4) AS avg_tps
FROM txs
GROUP BY hour
ORDER BY hour DESC
LIMIT 48
SQL
```

### 9. Top senders

```sql
SELECT `from`, count() AS tx_count
FROM txs
GROUP BY `from`
ORDER BY tx_count DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT `from`, count() AS tx_count
FROM txs
GROUP BY `from`
ORDER BY tx_count DESC
LIMIT 20
SQL
```

### 10. Top recipients or contracts by transaction count

```sql
SELECT `to`, count() AS tx_count
FROM txs
WHERE `to` IS NOT NULL
GROUP BY `to`
ORDER BY tx_count DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT `to`, count() AS tx_count
FROM txs
WHERE `to` IS NOT NULL
GROUP BY `to`
ORDER BY tx_count DESC
LIMIT 20
SQL
```

### 11. Top contracts by emitted logs

```sql
SELECT address, count() AS log_count
FROM logs
GROUP BY address
ORDER BY log_count DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT address, count() AS log_count
FROM logs
GROUP BY address
ORDER BY log_count DESC
LIMIT 20
SQL
```

### 12. Top event signatures

```sql
SELECT topic0, count() AS log_count
FROM logs
WHERE topic0 IS NOT NULL
GROUP BY topic0
ORDER BY log_count DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT topic0, count() AS log_count
FROM logs
WHERE topic0 IS NOT NULL
GROUP BY topic0
ORDER BY log_count DESC
LIMIT 20
SQL
```

### 13. Top method selectors

```sql
SELECT substring(input, 1, 10) AS selector, count() AS tx_count
FROM txs
WHERE length(input) >= 10
GROUP BY selector
ORDER BY tx_count DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT substring(input, 1, 10) AS selector, count() AS tx_count
FROM txs
WHERE length(input) >= 10
GROUP BY selector
ORDER BY tx_count DESC
LIMIT 20
SQL
```

### 14. Recent contract deployments

```sql
SELECT block_num, tx_hash, contract_address, `from`
FROM receipts
WHERE contract_address IS NOT NULL
ORDER BY block_num DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT block_num, tx_hash, contract_address, `from`
FROM receipts
WHERE contract_address IS NOT NULL
ORDER BY block_num DESC
LIMIT 20
SQL
```

### 15. Biggest gas users

```sql
SELECT block_num, tx_hash, gas_used, `from`, `to`
FROM receipts
ORDER BY gas_used DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT block_num, tx_hash, gas_used, `from`, `to`
FROM receipts
ORDER BY gas_used DESC
LIMIT 20
SQL
```

### 16. Contracts with highest total gas consumed

```sql
SELECT `to`, sum(gas_used) AS total_gas, count() AS tx_count
FROM receipts
WHERE `to` IS NOT NULL
GROUP BY `to`
ORDER BY total_gas DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT `to`, sum(gas_used) AS total_gas, count() AS tx_count
FROM receipts
WHERE `to` IS NOT NULL
GROUP BY `to`
ORDER BY total_gas DESC
LIMIT 20
SQL
```

### 17. Most active addresses in the last 24h

```sql
SELECT address, count() AS appearances
FROM
(
  SELECT `from` AS address
  FROM txs
  WHERE block_timestamp >= now() - INTERVAL 1 DAY

  UNION ALL

  SELECT `to` AS address
  FROM txs
  WHERE `to` IS NOT NULL
    AND block_timestamp >= now() - INTERVAL 1 DAY
)
GROUP BY address
ORDER BY appearances DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT address, count() AS appearances
FROM
(
  SELECT `from` AS address
  FROM txs
  WHERE block_timestamp >= now() - INTERVAL 1 DAY

  UNION ALL

  SELECT `to` AS address
  FROM txs
  WHERE `to` IS NOT NULL
    AND block_timestamp >= now() - INTERVAL 1 DAY
)
GROUP BY address
ORDER BY appearances DESC
LIMIT 20
SQL
```

### 18. ERC-20 style transfer-heavy contracts

```sql
SELECT address, count() AS transfers
FROM logs
WHERE topic0 = '0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef'
GROUP BY address
ORDER BY transfers DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT address, count() AS transfers
FROM logs
WHERE topic0 = '0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef'
GROUP BY address
ORDER BY transfers DESC
LIMIT 20
SQL
```

### 19. ERC-20 style approval-heavy contracts

```sql
SELECT address, count() AS approvals
FROM logs
WHERE topic0 = '0x8c5be1e5ebec7d5bd14f714f27d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925'
GROUP BY address
ORDER BY approvals DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT address, count() AS approvals
FROM logs
WHERE topic0 = '0x8c5be1e5ebec7d5bd14f714f27d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925'
GROUP BY address
ORDER BY approvals DESC
LIMIT 20
SQL
```

### 20. Address activity drilldown

```sql
SELECT block_num, idx, hash, `from`, `to`, value, gas_used
FROM txs
WHERE `from` = '0xYOUR_ADDRESS'
   OR `to` = '0xYOUR_ADDRESS'
ORDER BY block_num DESC, idx DESC
LIMIT 100
```

```bash
cat <<'SQL' | run_ch_query
SELECT block_num, idx, hash, `from`, `to`, value, gas_used
FROM txs
WHERE `from` = '0xYOUR_ADDRESS'
   OR `to` = '0xYOUR_ADDRESS'
ORDER BY block_num DESC, idx DESC
LIMIT 100
SQL
```

## Recommended First Queries

- `5` for global totals
- `6` for daily transaction charts
- `8` for TPS trend
- `10` for top active contracts
- `11` for top log emitters
- `12` for top event signatures
- `13` for top called methods
- `18` for token transfer-heavy contracts
