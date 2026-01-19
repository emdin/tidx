-- Logs table
-- Partitioned by block range (2M blocks per partition)
CREATE TABLE IF NOT EXISTS logs (
    block_num       INT8 NOT NULL,
    block_timestamp TIMESTAMPTZ NOT NULL,
    log_idx         INT4 NOT NULL,
    tx_idx          INT4 NOT NULL,
    tx_hash         BYTEA NOT NULL,
    address         BYTEA NOT NULL,
    selector        BYTEA,
    topics          BYTEA[] NOT NULL,
    data            BYTEA NOT NULL,
    PRIMARY KEY (block_num, log_idx)
) PARTITION BY RANGE (block_num);

-- Default partition for initial development (covers 0-2M)
CREATE TABLE IF NOT EXISTS logs_b0m PARTITION OF logs
    FOR VALUES FROM (0) TO (2000000);

-- Fast selector queries (golden-axe pattern)
CREATE INDEX IF NOT EXISTS idx_logs_selector ON logs (selector, block_timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_logs_address ON logs (address, block_timestamp DESC);
