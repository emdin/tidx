-- Blocks table (Tempo-specific: no is_canonical needed due to instant finality)
-- Partitioned by block range (2M blocks per partition)
CREATE TABLE IF NOT EXISTS blocks (
    num             INT8 NOT NULL,
    hash            BYTEA NOT NULL,
    parent_hash     BYTEA NOT NULL,
    timestamp       TIMESTAMPTZ NOT NULL,
    timestamp_ms    INT8 NOT NULL,
    gas_limit       INT8 NOT NULL,
    gas_used        INT8 NOT NULL,
    miner           BYTEA NOT NULL,
    extra_data      BYTEA,
    PRIMARY KEY (num)
) PARTITION BY RANGE (num);

-- Default partition for initial development (covers 0-2M)
CREATE TABLE IF NOT EXISTS blocks_b0m PARTITION OF blocks
    FOR VALUES FROM (0) TO (2000000);

-- Block lookups
CREATE INDEX IF NOT EXISTS idx_blocks_hash ON blocks (hash);
CREATE INDEX IF NOT EXISTS idx_blocks_timestamp ON blocks (timestamp DESC);
