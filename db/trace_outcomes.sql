-- Per-transaction trace outcome. Answers "was this tx traced, and did it
-- succeed?" — which `internal_txs` cannot: a trace that succeeds with zero
-- nested frames writes no rows there, and a failed trace writes nothing either.
--
--   ok      traced, ≥1 nested frame written to internal_txs
--   empty   traced, no nested frames (leaf call / plain transfer)
--   failed  every attempt errored; `error` holds the last message. Retried by
--           the engine's trace-repair loop and by `tidx backfill-traces`.
--
-- A tx with NO row here has never been traced (e.g. indexed before this table
-- existed, or by a path that had tracing disabled).
CREATE TABLE IF NOT EXISTS trace_outcomes (
    tx_hash    BYTEA PRIMARY KEY,
    block_num  INT8 NOT NULL,
    outcome    TEXT NOT NULL CHECK (outcome IN ('ok', 'empty', 'failed')),
    frames     INT4 NOT NULL DEFAULT 0,
    -- cumulative RPC attempts across all passes
    attempts   INT4 NOT NULL DEFAULT 1,
    error      TEXT,
    traced_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_trace_outcomes_block_num ON trace_outcomes (block_num);
-- Repair loop scans only failures; keep that index tiny.
CREATE INDEX IF NOT EXISTS idx_trace_outcomes_failed ON trace_outcomes (block_num) WHERE outcome = 'failed';
