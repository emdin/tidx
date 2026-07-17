-- Reorg archive: when tidx handles a reorg, displaced rows are moved into
-- orphaned_* tables instead of hard-deleted, and a reorgs row records the event.
-- Idempotent: safe to re-run (schema.rs invokes on every boot).
-- Down script: db/undo_reorg_archive.sql (not auto-run; execute manually to roll back).

CREATE TABLE IF NOT EXISTS reorgs (
  id                    BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  fork_point            BIGINT NOT NULL,
  prev_tip              BIGINT NOT NULL,
  depth                 INT    NOT NULL,
  blocks_removed        INT    NOT NULL,
  txs_removed           INT    NOT NULL,
  logs_removed          INT    NOT NULL,
  receipts_removed      INT    NOT NULL,
  internal_txs_removed  INT    NOT NULL,
  withdrawals_removed   INT    NOT NULL,
  count_check_ok        BOOLEAN NOT NULL DEFAULT true,
  occurred_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS orphaned_txs (
  reorg_id    BIGINT NOT NULL REFERENCES reorgs(id),
  orphaned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  LIKE txs
);

CREATE TABLE IF NOT EXISTS orphaned_logs (
  reorg_id    BIGINT NOT NULL REFERENCES reorgs(id),
  orphaned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  LIKE logs
);

CREATE TABLE IF NOT EXISTS orphaned_receipts (
  reorg_id    BIGINT NOT NULL REFERENCES reorgs(id),
  orphaned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  LIKE receipts
);

CREATE TABLE IF NOT EXISTS orphaned_internal_txs (
  reorg_id    BIGINT NOT NULL REFERENCES reorgs(id),
  orphaned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  LIKE internal_txs
);

CREATE TABLE IF NOT EXISTS orphaned_l2_withdrawals (
  reorg_id    BIGINT NOT NULL REFERENCES reorgs(id),
  orphaned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  LIKE l2_withdrawals
);

CREATE TABLE IF NOT EXISTS orphaned_blocks (
  reorg_id    BIGINT NOT NULL REFERENCES reorgs(id),
  orphaned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  LIKE blocks
);

CREATE INDEX IF NOT EXISTS orphaned_txs_hash               ON orphaned_txs (hash);
CREATE INDEX IF NOT EXISTS orphaned_txs_from               ON orphaned_txs ("from");
CREATE INDEX IF NOT EXISTS orphaned_txs_reorg              ON orphaned_txs (reorg_id);
CREATE INDEX IF NOT EXISTS orphaned_txs_block_num          ON orphaned_txs (block_num);

CREATE INDEX IF NOT EXISTS orphaned_logs_tx_hash           ON orphaned_logs (tx_hash);
CREATE INDEX IF NOT EXISTS orphaned_logs_reorg             ON orphaned_logs (reorg_id);

CREATE INDEX IF NOT EXISTS orphaned_receipts_tx_hash       ON orphaned_receipts (tx_hash);
CREATE INDEX IF NOT EXISTS orphaned_receipts_reorg         ON orphaned_receipts (reorg_id);

CREATE INDEX IF NOT EXISTS orphaned_internal_txs_reorg     ON orphaned_internal_txs (reorg_id);

CREATE INDEX IF NOT EXISTS orphaned_l2_withdrawals_reorg   ON orphaned_l2_withdrawals (reorg_id);

CREATE INDEX IF NOT EXISTS orphaned_blocks_num             ON orphaned_blocks (num);
CREATE INDEX IF NOT EXISTS orphaned_blocks_reorg           ON orphaned_blocks (reorg_id);
