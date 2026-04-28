CREATE TABLE IF NOT EXISTS internal_txs (
    block_num       Int64,
    block_timestamp DateTime64(3, 'UTC'),
    tx_idx          Int32,
    tx_hash         String,
    depth           Int32,
    path_idx        Int32,
    call_type       String,
    `from`          String,
    `to`            Nullable(String),
    value           String,
    -- UInt256 mirror of value, matches the txs/receipts pattern from phase 3.
    value_u256      UInt256 DEFAULT toUInt256OrZero(value),
    input           String,
    input_selector  String DEFAULT '',
    output          String,
    gas_used        Int64,
    error           Nullable(String),

    INDEX idx_tx_hash tx_hash TYPE bloom_filter GRANULARITY 1,
    INDEX idx_from `from` TYPE bloom_filter GRANULARITY 1,
    INDEX idx_to `to` TYPE bloom_filter GRANULARITY 1,
    INDEX idx_selector input_selector TYPE bloom_filter GRANULARITY 1
) ENGINE = ReplacingMergeTree()
PARTITION BY toYYYYMM(block_timestamp)
ORDER BY (block_num, tx_idx, path_idx)
