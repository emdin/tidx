-- TimescaleDB Hypertables Migration (for fresh installs)
-- 
-- This migration converts tables to TimescaleDB hypertables with compression.
-- It must be run INSTEAD OF the partitioned table migrations (001-003) for
-- fresh installs that want full TimescaleDB features.
--
-- Usage: For fresh TimescaleDB installs, run this instead of 001-005.

-- ============================================================================
-- BLOCKS HYPERTABLE
-- ============================================================================

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
);

-- Convert to hypertable with 2M block chunks
SELECT create_hypertable('blocks', by_range('num', 2000000), if_not_exists => TRUE);

-- Block indexes
CREATE INDEX IF NOT EXISTS idx_blocks_hash ON blocks (hash);
CREATE INDEX IF NOT EXISTS idx_blocks_timestamp ON blocks (timestamp DESC);

-- Enable compression
ALTER TABLE blocks SET (
    timescaledb.compress,
    timescaledb.compress_orderby = 'num DESC'
);

-- Auto-compress chunks older than 2M blocks (one chunk)
SELECT add_compression_policy('blocks', 2000000, if_not_exists => TRUE);

-- ============================================================================
-- TRANSACTIONS HYPERTABLE
-- ============================================================================

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
);

-- Convert to hypertable
SELECT create_hypertable('txs', by_range('block_num', 2000000), if_not_exists => TRUE);

-- Transaction indexes
CREATE INDEX IF NOT EXISTS idx_txs_hash ON txs (hash);
CREATE INDEX IF NOT EXISTS idx_txs_from ON txs ("from", block_timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_txs_to ON txs ("to", block_timestamp DESC);

-- Enable compression (segment by type for better ratio)
ALTER TABLE txs SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'type',
    timescaledb.compress_orderby = 'block_num DESC, idx'
);

SELECT add_compression_policy('txs', 2000000, if_not_exists => TRUE);

-- ============================================================================
-- LOGS HYPERTABLE
-- ============================================================================

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
);

-- Convert to hypertable
SELECT create_hypertable('logs', by_range('block_num', 2000000), if_not_exists => TRUE);

-- Log indexes
CREATE INDEX IF NOT EXISTS idx_logs_selector ON logs (selector, block_timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_logs_address ON logs (address, block_timestamp DESC);

-- Enable compression (segment by selector for event queries)
ALTER TABLE logs SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'selector',
    timescaledb.compress_orderby = 'block_num DESC, log_idx'
);

SELECT add_compression_policy('logs', 2000000, if_not_exists => TRUE);

-- ============================================================================
-- SYNC STATE
-- ============================================================================

CREATE TABLE IF NOT EXISTS sync_state (
    id              INT4 PRIMARY KEY DEFAULT 1,
    chain_id        INT8 NOT NULL,
    head_num        INT8 NOT NULL DEFAULT 0,
    synced_num      INT8 NOT NULL DEFAULT 0,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (id = 1)
);

-- ============================================================================
-- NOTES ON MATERIALIZED VIEWS
-- ============================================================================
-- With columnar compression, direct queries on hypertables are already fast.
-- Materialized views are NOT needed for most use cases.
-- 
-- If you need pre-aggregated data for dashboards, create them manually:
--
--   CREATE MATERIALIZED VIEW txs_hourly AS
--   SELECT date_trunc('hour', block_timestamp) AS bucket, type, COUNT(*)
--   FROM txs GROUP BY bucket, type;
--
--   REFRESH MATERIALIZED VIEW txs_hourly;
