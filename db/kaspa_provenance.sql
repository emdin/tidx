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

-- L1 miner fee, in sompi, paid by the carrier tx = sum(inputs) - sum(outputs).
-- Filled either at insertion time by the realtime writer (from the block data
-- it already has) or after-the-fact by the enrich-l1-fees CLI walking kaspad.
-- Nullable — NULL means "not enriched yet." Backfill is scoped to whatever
-- kaspad-mainnet's retention window covers (default 30d, currently 130d).
ALTER TABLE kaspa_entries
    ADD COLUMN IF NOT EXISTS l1_fee_sompi         INT8,
    ADD COLUMN IF NOT EXISTS l1_fee_enriched_at   TIMESTAMPTZ;

ALTER TABLE kaspa_l2_submissions
    ADD COLUMN IF NOT EXISTS l1_fee_sompi         INT8,
    ADD COLUMN IF NOT EXISTS l1_fee_enriched_at   TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_kaspa_entries_fee_pending
    ON kaspa_entries (kaspa_txid)
    WHERE l1_fee_sompi IS NULL;

CREATE INDEX IF NOT EXISTS idx_kaspa_l2_submissions_fee_pending
    ON kaspa_l2_submissions (kaspa_txid)
    WHERE l1_fee_sompi IS NULL;

-- kaspa_tx_index: (txid → block_hash) mapping for every Kaspa tx we observe.
-- The minimum missing primitive vs kaspad's native RPC: without this we can
-- only get a tx by walking blocks, but *with* this any tx-by-id lookup is
-- one PG query + one kaspad getBlock. Enables local fee computation
-- (fee = sum(inputs.previousOutpoint amounts) - sum(outputs.amounts))
-- without an external API dependency.
--
-- Populated by:
--  (1) realtime sync — every block processed, upsert its full tx list
--  (2) backfill CLI — walk kaspad backward for the retention window
--
-- Storage: ~40 bytes/row (txid + block_hash + PK overhead). At ~5 txs/block
-- × Kaspa's ~10 bps, that's ~4M rows/day = ~160 MB/day. 30-day window ~5 GB.
CREATE TABLE IF NOT EXISTS kaspa_tx_index (
    txid       BYTEA PRIMARY KEY,
    block_hash BYTEA NOT NULL,
    -- The accepting block's daaScore, if known. Helps age-based pruning +
    -- retention-boundary checks (skip enrichment for txs below the window).
    daa_score  INT8,
    seen_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_kaspa_tx_index_block_hash
    ON kaspa_tx_index (block_hash);

CREATE INDEX IF NOT EXISTS idx_kaspa_tx_index_daa_score
    ON kaspa_tx_index (daa_score)
    WHERE daa_score IS NOT NULL;

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
