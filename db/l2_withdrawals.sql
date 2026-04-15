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
