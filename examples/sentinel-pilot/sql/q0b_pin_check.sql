-- Q0b: reorg detection on resume / before each poll. Compare to the stored
-- hash for the pinned block. Same -> continue from the checkpoint. Different ->
-- everything observed above the fork point must be replayed (q0c lists what
-- was orphaned) and prior alerts in that range corrected, not silently dropped.
-- Also fetch TS_LO for a window with this query (num = BLOCK_LO).
SELECT num, hash, timestamp
FROM blocks
WHERE num = {{PINNED_BLOCK}}
