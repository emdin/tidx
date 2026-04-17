CREATE TABLE IF NOT EXISTS blocks (
    num             Int64,
    hash            String,
    parent_hash     String,
    timestamp       DateTime64(3, 'UTC'),
    timestamp_ms    Int64,
    real_timestamp  Nullable(DateTime64(3, 'UTC')),
    real_timestamp_ms Nullable(Int64),
    timestamp_drift_secs Nullable(Int32),
    l1_block_count  Nullable(Int16),
    l1_last_daa_score Nullable(Int64),
    parent_beacon_block_root Nullable(String),
    gas_limit       Int64,
    gas_used        Int64,
    miner           String,
    extra_data      Nullable(String)
) ENGINE = ReplacingMergeTree()
PARTITION BY toYYYYMM(timestamp)
ORDER BY (num)
