CREATE TABLE IF NOT EXISTS l2_withdrawals (
    block_num           Int64,
    block_timestamp     DateTime64(3, 'UTC'),
    idx                 Int32,
    withdrawal_index    String,
    index_le            String,
    validator_index     String,
    address             String,
    amount_gwei         Int64,
    amount_sompi        Int64
) ENGINE = ReplacingMergeTree()
PARTITION BY toYYYYMM(block_timestamp)
ORDER BY (block_num, idx)
