-- Transactions table (Tempo 0x76 native)
-- Partitioned by block range (2M blocks per partition)
CREATE TABLE IF NOT EXISTS txs (
    block_num               INT8 NOT NULL,
    block_timestamp         TIMESTAMPTZ NOT NULL,
    idx                     INT4 NOT NULL,
    hash                    BYTEA NOT NULL,
    type                    INT2 NOT NULL,
    "from"                  BYTEA NOT NULL,
    "to"                    BYTEA,
    value                   TEXT NOT NULL,
    input                   BYTEA NOT NULL,
    gas_limit               INT8 NOT NULL,
    max_fee_per_gas         TEXT NOT NULL,
    max_priority_fee_per_gas TEXT NOT NULL,
    gas_used                INT8,
    nonce_key               BYTEA NOT NULL,
    nonce                   INT8 NOT NULL,
    fee_token               BYTEA,
    fee_payer               BYTEA,
    calls                   JSONB,
    call_count              INT2 NOT NULL DEFAULT 1,
    valid_before            INT8,
    valid_after             INT8,
    signature_type          INT2,
    PRIMARY KEY (block_num, idx)
) PARTITION BY RANGE (block_num);

-- Default partition for initial development (covers 0-2M)
CREATE TABLE IF NOT EXISTS txs_b0m PARTITION OF txs
    FOR VALUES FROM (0) TO (2000000);

-- Transaction lookups
CREATE INDEX IF NOT EXISTS idx_txs_hash ON txs (hash);
CREATE INDEX IF NOT EXISTS idx_txs_from ON txs ("from", block_timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_txs_to ON txs ("to", block_timestamp DESC);
