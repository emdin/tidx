CREATE TABLE IF NOT EXISTS kaspa_provenance_meta (
    id                          BOOLEAN PRIMARY KEY DEFAULT TRUE,
    chain_id                    INT8 NOT NULL,
    kaspa_rpc_url               TEXT NOT NULL,
    txid_prefix                 BYTEA NOT NULL,
    promotion_delay_secs        INT8 NOT NULL,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (id = TRUE)
);

CREATE TABLE IF NOT EXISTS kaspa_sync_state (
    id                          BOOLEAN PRIMARY KEY DEFAULT TRUE,
    checkpoint_hash             BYTEA,
    last_seen_sink              BYTEA,
    last_virtual_daa_score      INT8,
    tip_distance                INT8 NOT NULL DEFAULT 100,
    last_success_at             TIMESTAMPTZ,
    last_error                  TEXT,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (id = TRUE)
);

CREATE TABLE IF NOT EXISTS kaspa_pending_l2_submissions (
    l2_tx_hash                  BYTEA PRIMARY KEY,
    kaspa_txid                  BYTEA NOT NULL UNIQUE,
    accepted_chain_block_hash   BYTEA NOT NULL,
    accepted_at                 TIMESTAMPTZ NOT NULL,
    promote_after               TIMESTAMPTZ NOT NULL,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_kaspa_pending_l2_promote_after
    ON kaspa_pending_l2_submissions (promote_after);

CREATE INDEX IF NOT EXISTS idx_kaspa_pending_l2_accepted_block
    ON kaspa_pending_l2_submissions (accepted_chain_block_hash);

CREATE TABLE IF NOT EXISTS kaspa_pending_entries (
    kaspa_txid                  BYTEA PRIMARY KEY,
    recipient                   BYTEA NOT NULL,
    amount_sompi                INT8 NOT NULL,
    accepted_chain_block_hash   BYTEA NOT NULL,
    accepted_at                 TIMESTAMPTZ NOT NULL,
    promote_after               TIMESTAMPTZ NOT NULL,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_kaspa_pending_entries_promote_after
    ON kaspa_pending_entries (promote_after);

CREATE INDEX IF NOT EXISTS idx_kaspa_pending_entries_accepted_block
    ON kaspa_pending_entries (accepted_chain_block_hash);

CREATE TABLE IF NOT EXISTS kaspa_l2_submissions (
    l2_tx_hash                  BYTEA PRIMARY KEY,
    kaspa_txid                  BYTEA NOT NULL UNIQUE,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_kaspa_l2_submissions_kaspa_txid
    ON kaspa_l2_submissions (kaspa_txid);

CREATE TABLE IF NOT EXISTS kaspa_entries (
    kaspa_txid                  BYTEA PRIMARY KEY,
    recipient                   BYTEA NOT NULL,
    amount_sompi                INT8 NOT NULL,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_kaspa_entries_recipient
    ON kaspa_entries (recipient);

CREATE TABLE IF NOT EXISTS kaspa_provenance_gaps (
    id                          BIGSERIAL PRIMARY KEY,
    from_checkpoint_hash        BYTEA,
    to_observed_hash            BYTEA,
    reason                      TEXT NOT NULL,
    started_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at                 TIMESTAMPTZ,
    details                     JSONB
);

-- L1 sender resolution columns (Phase 1 enrichment).
-- All nullable; NULL means "not enriched yet". Backfill is performed by the
-- enrich-kaspa-senders CLI, which scans rows with l1_senders IS NULL.
ALTER TABLE kaspa_entries
    ADD COLUMN IF NOT EXISTS l1_senders              TEXT[],
    ADD COLUMN IF NOT EXISTS l1_sender_amounts_sompi INT8[],
    ADD COLUMN IF NOT EXISTS l1_enriched_at          TIMESTAMPTZ;

ALTER TABLE kaspa_l2_submissions
    ADD COLUMN IF NOT EXISTS l1_senders              TEXT[],
    ADD COLUMN IF NOT EXISTS l1_sender_amounts_sompi INT8[],
    ADD COLUMN IF NOT EXISTS l1_enriched_at          TIMESTAMPTZ;

-- GIN partial indexes for "find all enriched rows that involve L1 address X".
CREATE INDEX IF NOT EXISTS idx_kaspa_entries_l1_senders_gin
    ON kaspa_entries USING gin (l1_senders)
    WHERE l1_senders IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_kaspa_l2_submissions_l1_senders_gin
    ON kaspa_l2_submissions USING gin (l1_senders)
    WHERE l1_senders IS NOT NULL;

-- Work-queue partial indexes: speed up the enrichment scan, which is
-- naturally driven by `WHERE l1_senders IS NULL`.
CREATE INDEX IF NOT EXISTS idx_kaspa_entries_enrichment_pending
    ON kaspa_entries (kaspa_txid)
    WHERE l1_senders IS NULL;

CREATE INDEX IF NOT EXISTS idx_kaspa_l2_submissions_enrichment_pending
    ON kaspa_l2_submissions (kaspa_txid)
    WHERE l1_senders IS NULL;
