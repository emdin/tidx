CREATE TABLE IF NOT EXISTS l2_withdrawals (
    block_num           INT8 NOT NULL,
    block_timestamp     TIMESTAMPTZ NOT NULL,
    idx                 INT4 NOT NULL,
    withdrawal_index    TEXT NOT NULL,
    index_le            BYTEA NOT NULL,
    validator_index     TEXT NOT NULL,
    address             BYTEA NOT NULL,
    amount_gwei         INT8 NOT NULL,
    amount_sompi        INT8 NOT NULL,
    PRIMARY KEY (block_num, idx)
);

CREATE INDEX IF NOT EXISTS idx_l2_withdrawals_address
    ON l2_withdrawals (address, block_num DESC);

CREATE INDEX IF NOT EXISTS idx_l2_withdrawals_amount_sompi
    ON l2_withdrawals (amount_sompi);

CREATE INDEX IF NOT EXISTS idx_l2_withdrawals_index_le
    ON l2_withdrawals (index_le);

-- Phase 1: link each L2 withdrawal (which is actually an Igra entry — see
-- decode_withdrawals in src/sync/decoder.rs) back to its originating Kaspa tx.
-- Backfilled lazily by the link-l2-withdrawals CLI; NULL until then.
ALTER TABLE l2_withdrawals
    ADD COLUMN IF NOT EXISTS kaspa_txid BYTEA;

CREATE INDEX IF NOT EXISTS idx_l2_withdrawals_kaspa_txid
    ON l2_withdrawals (kaspa_txid)
    WHERE kaspa_txid IS NOT NULL;
