# Investigator Query Pack

This file contains 20 tested ClickHouse queries for security and financial investigations on `igra`.

Base endpoint:

```bash
https://explorer.igralabs.com/query
```

Tested sample entities:

- Wallet under investigation: `0x123ddd6ebfafb687fee659a9f2f1d25298c965e8`
- High-traffic contract: `0xc24df70e408739aef6bf594fd41db4632df49188`
- Token contract: `0x69791fe346567c11941644ea53f556264a27b8d6`
- Wallet topic form for log filters: `0x000000000000000000000000123ddd6ebfafb687fee659a9f2f1d25298c965e8`

Notes:

- These queries were originally validated against staging; re-run them against production after first deploy.
- Replace the sample wallet or contract values for real investigations.
- Log topic filters use 32-byte topic encoding, so wallet addresses in `topic1/topic2` must be left-padded.

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

## 1. Wallet Activity Timeline

Shows the transaction history for the investigated wallet.

```sql
SELECT block_num, idx, hash, `from`, `to`, value, gas_used
FROM txs
WHERE `from` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
   OR `to` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
ORDER BY block_num DESC, idx DESC
LIMIT 100
```

```bash
cat <<'SQL' | run_ch_query
SELECT block_num, idx, hash, `from`, `to`, value, gas_used
FROM txs
WHERE `from` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
   OR `to` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
ORDER BY block_num DESC, idx DESC
LIMIT 100
SQL
```

## 2. Wallet Counterparties

Top addresses the wallet interacts with.

```sql
SELECT counterparty, count() AS interactions
FROM
(
  SELECT `to` AS counterparty
  FROM txs
  WHERE `from` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
    AND `to` IS NOT NULL

  UNION ALL

  SELECT `from` AS counterparty
  FROM txs
  WHERE `to` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
)
GROUP BY counterparty
ORDER BY interactions DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT counterparty, count() AS interactions
FROM
(
  SELECT `to` AS counterparty
  FROM txs
  WHERE `from` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
    AND `to` IS NOT NULL

  UNION ALL

  SELECT `from` AS counterparty
  FROM txs
  WHERE `to` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
)
GROUP BY counterparty
ORDER BY interactions DESC
LIMIT 20
SQL
```

## 3. Wallet First Seen / Last Seen

Establishes the active time window for a wallet.

```sql
SELECT min(block_timestamp) AS first_seen, max(block_timestamp) AS last_seen, count() AS tx_count
FROM txs
WHERE `from` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
   OR `to` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
```

```bash
cat <<'SQL' | run_ch_query
SELECT min(block_timestamp) AS first_seen, max(block_timestamp) AS last_seen, count() AS tx_count
FROM txs
WHERE `from` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
   OR `to` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
SQL
```

## 4. Wallet Failed Transactions

Useful for exploit attempts, broken bots, or reverted interaction analysis.

```sql
SELECT receipts.block_num, receipts.tx_hash, receipts.status, receipts.gas_used, txs.`to`
FROM receipts
INNER JOIN txs ON receipts.tx_hash = txs.hash
WHERE (txs.`from` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
    OR txs.`to` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8')
  AND receipts.status = 0
ORDER BY receipts.block_num DESC
LIMIT 50
```

```bash
cat <<'SQL' | run_ch_query
SELECT receipts.block_num, receipts.tx_hash, receipts.status, receipts.gas_used, txs.`to`
FROM receipts
INNER JOIN txs ON receipts.tx_hash = txs.hash
WHERE (txs.`from` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
    OR txs.`to` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8')
  AND receipts.status = 0
ORDER BY receipts.block_num DESC
LIMIT 50
SQL
```

## 5. Wallet Burst Activity By Minute

Finds spam or coordinated burst behavior from a wallet.

```sql
SELECT toStartOfMinute(block_timestamp) AS minute, count() AS txs
FROM txs
WHERE `from` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
GROUP BY minute
ORDER BY txs DESC, minute DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT toStartOfMinute(block_timestamp) AS minute, count() AS txs
FROM txs
WHERE `from` = '0x123ddd6ebfafb687fee659a9f2f1d25298c965e8'
GROUP BY minute
ORDER BY txs DESC, minute DESC
LIMIT 20
SQL
```

## 6. Top Callers To A Contract

Who is hitting a contract most often.

```sql
SELECT `from`, count() AS calls
FROM txs
WHERE `to` = '0xc24df70e408739aef6bf594fd41db4632df49188'
GROUP BY `from`
ORDER BY calls DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT `from`, count() AS calls
FROM txs
WHERE `to` = '0xc24df70e408739aef6bf594fd41db4632df49188'
GROUP BY `from`
ORDER BY calls DESC
LIMIT 20
SQL
```

## 7. Top Method Selectors To A Contract

Which functions dominate usage of the contract.

```sql
SELECT substring(input, 1, 10) AS selector, count() AS calls
FROM txs
WHERE `to` = '0xc24df70e408739aef6bf594fd41db4632df49188'
  AND length(input) >= 10
GROUP BY selector
ORDER BY calls DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT substring(input, 1, 10) AS selector, count() AS calls
FROM txs
WHERE `to` = '0xc24df70e408739aef6bf594fd41db4632df49188'
  AND length(input) >= 10
GROUP BY selector
ORDER BY calls DESC
LIMIT 20
SQL
```

## 8. Failed Calls To A Contract By Selector

Shows which methods are failing most often.

```sql
SELECT substring(txs.input, 1, 10) AS selector, count() AS failed_calls
FROM receipts
INNER JOIN txs ON receipts.tx_hash = txs.hash
WHERE txs.`to` = '0xc24df70e408739aef6bf594fd41db4632df49188'
  AND receipts.status = 0
  AND length(txs.input) >= 10
GROUP BY selector
ORDER BY failed_calls DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT substring(txs.input, 1, 10) AS selector, count() AS failed_calls
FROM receipts
INNER JOIN txs ON receipts.tx_hash = txs.hash
WHERE txs.`to` = '0xc24df70e408739aef6bf594fd41db4632df49188'
  AND receipts.status = 0
  AND length(txs.input) >= 10
GROUP BY selector
ORDER BY failed_calls DESC
LIMIT 20
SQL
```

## 9. Top Events Emitted By A Contract

Useful for behavioral fingerprinting of a contract.

```sql
SELECT topic0, count() AS emitted
FROM logs
WHERE address = '0xc24df70e408739aef6bf594fd41db4632df49188'
  AND topic0 IS NOT NULL
GROUP BY topic0
ORDER BY emitted DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT topic0, count() AS emitted
FROM logs
WHERE address = '0xc24df70e408739aef6bf594fd41db4632df49188'
  AND topic0 IS NOT NULL
GROUP BY topic0
ORDER BY emitted DESC
LIMIT 20
SQL
```

## 10. High-Gas Calls To A Contract

Good for exploit, liquidation, or expensive path analysis.

```sql
SELECT receipts.block_num, receipts.tx_hash, receipts.gas_used, txs.`from`, substring(txs.input, 1, 10) AS selector
FROM receipts
INNER JOIN txs ON receipts.tx_hash = txs.hash
WHERE txs.`to` = '0xc24df70e408739aef6bf594fd41db4632df49188'
ORDER BY receipts.gas_used DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT receipts.block_num, receipts.tx_hash, receipts.gas_used, txs.`from`, substring(txs.input, 1, 10) AS selector
FROM receipts
INNER JOIN txs ON receipts.tx_hash = txs.hash
WHERE txs.`to` = '0xc24df70e408739aef6bf594fd41db4632df49188'
ORDER BY receipts.gas_used DESC
LIMIT 20
SQL
```

## 11. Recent Contract Deployments

Tracks newly deployed contracts.

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

## 12. Top Deployers

Find the most prolific contract deployers.

```sql
SELECT `from`, count() AS contracts_deployed
FROM receipts
WHERE contract_address IS NOT NULL
GROUP BY `from`
ORDER BY contracts_deployed DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT `from`, count() AS contracts_deployed
FROM receipts
WHERE contract_address IS NOT NULL
GROUP BY `from`
ORDER BY contracts_deployed DESC
LIMIT 20
SQL
```

## 13. Token Transfers Involving A Wallet

Wallet-focused ERC-20 transfer investigation.

```sql
SELECT block_num, tx_hash, address, topic1, topic2
FROM logs
WHERE address = '0x69791fe346567c11941644ea53f556264a27b8d6'
  AND topic0 = '0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef'
  AND (
    topic1 = '0x000000000000000000000000123ddd6ebfafb687fee659a9f2f1d25298c965e8'
    OR topic2 = '0x000000000000000000000000123ddd6ebfafb687fee659a9f2f1d25298c965e8'
  )
ORDER BY block_num DESC
LIMIT 100
```

```bash
cat <<'SQL' | run_ch_query
SELECT block_num, tx_hash, address, topic1, topic2
FROM logs
WHERE address = '0x69791fe346567c11941644ea53f556264a27b8d6'
  AND topic0 = '0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef'
  AND (
    topic1 = '0x000000000000000000000000123ddd6ebfafb687fee659a9f2f1d25298c965e8'
    OR topic2 = '0x000000000000000000000000123ddd6ebfafb687fee659a9f2f1d25298c965e8'
  )
ORDER BY block_num DESC
LIMIT 100
SQL
```

## 14. Top Token Recipients

Which accounts receive the most transfers for a token.

```sql
SELECT topic2 AS recipient_topic, count() AS transfers
FROM logs
WHERE address = '0x69791fe346567c11941644ea53f556264a27b8d6'
  AND topic0 = '0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef'
GROUP BY recipient_topic
ORDER BY transfers DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT topic2 AS recipient_topic, count() AS transfers
FROM logs
WHERE address = '0x69791fe346567c11941644ea53f556264a27b8d6'
  AND topic0 = '0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef'
GROUP BY recipient_topic
ORDER BY transfers DESC
LIMIT 20
SQL
```

## 15. Top Token Senders

Which accounts send the most transfers for a token.

```sql
SELECT topic1 AS sender_topic, count() AS transfers
FROM logs
WHERE address = '0x69791fe346567c11941644ea53f556264a27b8d6'
  AND topic0 = '0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef'
GROUP BY sender_topic
ORDER BY transfers DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT topic1 AS sender_topic, count() AS transfers
FROM logs
WHERE address = '0x69791fe346567c11941644ea53f556264a27b8d6'
  AND topic0 = '0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef'
GROUP BY sender_topic
ORDER BY transfers DESC
LIMIT 20
SQL
```

## 16. Token Approvals Involving A Wallet

Allowance investigations for a token and wallet.

```sql
SELECT block_num, tx_hash, topic1, topic2
FROM logs
WHERE address = '0x69791fe346567c11941644ea53f556264a27b8d6'
  AND topic0 = '0x8c5be1e5ebec7d5bd14f714f27d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925'
  AND (
    topic1 = '0x000000000000000000000000123ddd6ebfafb687fee659a9f2f1d25298c965e8'
    OR topic2 = '0x000000000000000000000000123ddd6ebfafb687fee659a9f2f1d25298c965e8'
  )
ORDER BY block_num DESC
LIMIT 100
```

```bash
cat <<'SQL' | run_ch_query
SELECT block_num, tx_hash, topic1, topic2
FROM logs
WHERE address = '0x69791fe346567c11941644ea53f556264a27b8d6'
  AND topic0 = '0x8c5be1e5ebec7d5bd14f714f27d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925'
  AND (
    topic1 = '0x000000000000000000000000123ddd6ebfafb687fee659a9f2f1d25298c965e8'
    OR topic2 = '0x000000000000000000000000123ddd6ebfafb687fee659a9f2f1d25298c965e8'
  )
ORDER BY block_num DESC
LIMIT 100
SQL
```

## 17. Top Approval-Heavy Contracts

Useful for allowance risk hunting and phishing triage.

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

## 18. Same-Block Swarm Into A Contract

Spots coordinated many-caller bursts against a contract in one block.

```sql
SELECT txs.block_num, count() AS tx_count, uniqExact(txs.`from`) AS unique_callers
FROM txs
WHERE txs.`to` = '0xc24df70e408739aef6bf594fd41db4632df49188'
GROUP BY txs.block_num
HAVING tx_count >= 2
ORDER BY unique_callers DESC, tx_count DESC, txs.block_num DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT txs.block_num, count() AS tx_count, uniqExact(txs.`from`) AS unique_callers
FROM txs
WHERE txs.`to` = '0xc24df70e408739aef6bf594fd41db4632df49188'
GROUP BY txs.block_num
HAVING tx_count >= 2
ORDER BY unique_callers DESC, tx_count DESC, txs.block_num DESC
LIMIT 20
SQL
```

## 19. Repeated Sender-Contract Pairs

Surfaces recurring wallet-to-contract interaction pairs.

```sql
SELECT `from`, `to`, count() AS tx_count
FROM txs
WHERE `to` IS NOT NULL
GROUP BY `from`, `to`
ORDER BY tx_count DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT `from`, `to`, count() AS tx_count
FROM txs
WHERE `to` IS NOT NULL
GROUP BY `from`, `to`
ORDER BY tx_count DESC
LIMIT 20
SQL
```

## 20. Top Failed Senders

Shows the wallets generating the most reverted transactions.

```sql
SELECT txs.`from`, count() AS failed_txs
FROM receipts
INNER JOIN txs ON receipts.tx_hash = txs.hash
WHERE receipts.status = 0
GROUP BY txs.`from`
ORDER BY failed_txs DESC
LIMIT 20
```

```bash
cat <<'SQL' | run_ch_query
SELECT txs.`from`, count() AS failed_txs
FROM receipts
INNER JOIN txs ON receipts.tx_hash = txs.hash
WHERE receipts.status = 0
GROUP BY txs.`from`
ORDER BY failed_txs DESC
LIMIT 20
SQL
```
