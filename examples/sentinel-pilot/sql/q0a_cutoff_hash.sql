-- Q0a: pin the complete indexed cutoff.
-- `sync_state` is NOT in the /query allowlist, so: (1) GET /status ->
-- chains[0].synced_num (plus gap_blocks, gaps[], tip_num). synced_num is the
-- highest block below which tidx has verified the table contiguous (its
-- gap-fill loop only advances it after a bounded scan finds nothing). Never
-- scan past it. (2) pin its hash and store both with the checkpoint:
SELECT num AS cutoff, hash AS cutoff_hash, timestamp AS cutoff_ts
FROM blocks
WHERE num = {{CUTOFF}}
