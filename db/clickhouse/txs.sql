CREATE TABLE IF NOT EXISTS txs (
    block_num               Int64,
    block_timestamp         DateTime64(3, 'UTC'),
    idx                     Int32,
    hash                    String,
    `type`                  Int16,
    `from`                  String,
    `to`                    Nullable(String),
    value                   String,
    -- UInt256 mirror of `value` so callers can use direct numeric comparisons
    -- without `toUInt256OrZero()`. DEFAULT auto-converts on read; new rows are
    -- written with the same string and the DEFAULT applies.
    value_u256              UInt256 DEFAULT toUInt256OrZero(value),
    input                   String,
    selector                String DEFAULT '',
    gas_limit               Int64,
    max_fee_per_gas         String,
    max_fee_per_gas_u256    UInt256 DEFAULT toUInt256OrZero(max_fee_per_gas),
    max_priority_fee_per_gas String,
    max_priority_fee_per_gas_u256 UInt256 DEFAULT toUInt256OrZero(max_priority_fee_per_gas),
    gas_used                Nullable(Int64),
    nonce_key               String,
    nonce                   Int64,
    fee_token               Nullable(String),
    fee_payer               Nullable(String),
    calls                   Nullable(String),
    call_count              Int16,
    valid_before            Nullable(Int64),
    valid_after             Nullable(Int64),
    signature_type          Nullable(Int16),

    INDEX idx_selector selector TYPE bloom_filter GRANULARITY 1
) ENGINE = ReplacingMergeTree()
PARTITION BY toYYYYMM(block_timestamp)
ORDER BY (block_num, idx)
