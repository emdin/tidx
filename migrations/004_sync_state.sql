-- Sync state (single row per instance)
CREATE TABLE IF NOT EXISTS sync_state (
    id              INT4 PRIMARY KEY DEFAULT 1,
    chain_id        INT8 NOT NULL,
    head_num        INT8 NOT NULL DEFAULT 0,
    synced_num      INT8 NOT NULL DEFAULT 0,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT sync_state_single_row CHECK (id = 1)
);
