-- Tables for `tidx watch-tip`: every reorg seen at the chain tip, at any depth.
--
-- The engine indexes `head_delay_blocks` behind the tip, so the reorg archive
-- (reorg_archive.sql) only sees reorgs deeper than that window. This watcher
-- follows each RPC provider's tip on its own and records what that provider
-- replaced. Tables live in the `tipwatch` schema owned by the least-privilege
-- role from tipwatch_role.sql; the watcher creates them on first start.

-- Each provider's chain as last seen: the most recent `--memory-blocks` heights.
CREATE TABLE IF NOT EXISTS tipwatch.seen (
    provider    TEXT        NOT NULL,
    num         BIGINT      NOT NULL,
    hash        BYTEA       NOT NULL,
    parent_hash BYTEA       NOT NULL,
    block_ts    TIMESTAMPTZ NOT NULL,
    seen_at     TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (provider, num)
);

CREATE TABLE IF NOT EXISTS tipwatch.reorgs (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    provider    TEXT        NOT NULL,
    detected_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    fork_point  BIGINT      NOT NULL,
    old_tip     BIGINT      NOT NULL,
    new_tip     BIGINT      NOT NULL,
    -- Blocks of the old branch that were replaced.
    depth       INTEGER     NOT NULL,
    -- Found by comparing block numbers after a gap in observation (restart, or
    -- the tip jumped further than --max-walk): branches that came and went
    -- during the gap were not observable.
    after_gap   BOOLEAN     NOT NULL DEFAULT false,
    -- A later reorg brought this one's orphaned blocks back: the provider
    -- flapped between nodes on different branches, or the chain really
    -- switched back.
    reverted_by BIGINT      REFERENCES tipwatch.reorgs (id)
);
CREATE INDEX IF NOT EXISTS reorgs_detected_at ON tipwatch.reorgs (detected_at);

CREATE TABLE IF NOT EXISTS tipwatch.orphans (
    reorg_id    BIGINT      NOT NULL REFERENCES tipwatch.reorgs (id),
    provider    TEXT        NOT NULL,
    num         BIGINT      NOT NULL,
    hash        BYTEA       NOT NULL,
    parent_hash BYTEA       NOT NULL,
    block_ts    TIMESTAMPTZ NOT NULL,
    seen_at     TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (reorg_id, num)
);
CREATE INDEX IF NOT EXISTS orphans_provider_hash ON tipwatch.orphans (provider, hash);

-- Heights the watcher did not observe one by one (restart, a tip jump, or a
-- divergence deeper than its memory). Reorgs inside a gap can be missed.
CREATE TABLE IF NOT EXISTS tipwatch.gaps (
    provider TEXT        NOT NULL,
    at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    from_num BIGINT      NOT NULL,
    to_num   BIGINT      NOT NULL,
    reason   TEXT        NOT NULL
);
