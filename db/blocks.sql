CREATE TABLE IF NOT EXISTS blocks (
    num             INT8 NOT NULL,
    hash            BYTEA NOT NULL,
    parent_hash     BYTEA NOT NULL,
    timestamp       TIMESTAMPTZ NOT NULL,
    timestamp_ms    INT8 NOT NULL,
    real_timestamp  TIMESTAMPTZ,
    real_timestamp_ms INT8,
    timestamp_drift_secs INT4,
    l1_block_count  INT2,
    l1_last_daa_score INT8,
    parent_beacon_block_root BYTEA,
    gas_limit       INT8 NOT NULL,
    gas_used        INT8 NOT NULL,
    miner           BYTEA NOT NULL,
    extra_data      BYTEA,
    PRIMARY KEY (timestamp, num)
);

ALTER TABLE blocks ADD COLUMN IF NOT EXISTS real_timestamp TIMESTAMPTZ;
ALTER TABLE blocks ADD COLUMN IF NOT EXISTS real_timestamp_ms INT8;
ALTER TABLE blocks ADD COLUMN IF NOT EXISTS timestamp_drift_secs INT4;
ALTER TABLE blocks ADD COLUMN IF NOT EXISTS l1_block_count INT2;
ALTER TABLE blocks ADD COLUMN IF NOT EXISTS l1_last_daa_score INT8;
ALTER TABLE blocks ADD COLUMN IF NOT EXISTS parent_beacon_block_root BYTEA;

CREATE INDEX IF NOT EXISTS idx_blocks_num ON blocks (num DESC);
DROP INDEX IF EXISTS idx_blocks_num_asc;
CREATE INDEX IF NOT EXISTS idx_blocks_hash ON blocks (hash);
CREATE INDEX IF NOT EXISTS idx_blocks_timestamp ON blocks (timestamp);
CREATE INDEX IF NOT EXISTS idx_blocks_real_timestamp ON blocks (real_timestamp);
