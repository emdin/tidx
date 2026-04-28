-- Internal transactions: nested calls inside a transaction (depth ≥ 1).
-- Captured from `debug_traceTransaction` with the geth-style `callTracer`.
-- The top-level call (depth 0) is the tx itself and lives in `txs`.
CREATE TABLE IF NOT EXISTS internal_txs (
    block_num       INT8 NOT NULL,
    block_timestamp TIMESTAMPTZ NOT NULL,
    tx_idx          INT4 NOT NULL,
    tx_hash         BYTEA NOT NULL,
    -- 1 = direct nested, 2 = nested-in-nested, ...
    depth           INT4 NOT NULL,
    -- DFS pre-order index within the tx; uniquely identifies a frame.
    path_idx        INT4 NOT NULL,
    call_type       TEXT NOT NULL,
    "from"          BYTEA NOT NULL,
    "to"            BYTEA,
    -- uint256 wei amount as decimal string (matches `txs.value` format).
    value           TEXT NOT NULL,
    input           BYTEA NOT NULL,
    -- First 4 bytes of `input`, denormalized for indexed selector lookups.
    input_selector  BYTEA,
    output          BYTEA NOT NULL,
    gas_used        INT8 NOT NULL,
    error           TEXT,
    PRIMARY KEY (block_timestamp, block_num, tx_idx, path_idx)
);

CREATE INDEX IF NOT EXISTS idx_internal_txs_tx_hash ON internal_txs (tx_hash);
CREATE INDEX IF NOT EXISTS idx_internal_txs_block_num ON internal_txs (block_num DESC);
CREATE INDEX IF NOT EXISTS idx_internal_txs_from ON internal_txs ("from", block_timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_internal_txs_to ON internal_txs ("to", block_timestamp DESC) WHERE "to" IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_internal_txs_selector ON internal_txs (input_selector, block_timestamp DESC) WHERE input_selector IS NOT NULL;
-- Only-failed scan: filter by `error IS NOT NULL` is rare-enough that a
-- partial index pays off (most internal calls succeed).
CREATE INDEX IF NOT EXISTS idx_internal_txs_errors ON internal_txs (block_num DESC) WHERE error IS NOT NULL;
