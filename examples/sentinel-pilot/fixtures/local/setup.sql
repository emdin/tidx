-- Local-chain fixtures for cases Igra has NO real instance of (verified 2026-09-28):
--   F4   successful tx whose reverted internal subtree contains nonzero-value
--        children — 1 717 successful Igra txs contain a reverted frame, none
--        with value inside the subtree;
--   F4c  CREATE2 with value — 894 CREATE2 frames on Igra, 0 carry value;
--   F1c  seed → recipient → next recipient inside ONE block — none found on
--        Igra via direct transfer() over full history;
--   F10b an actual reorg — Igra's `reorgs` table has 0 rows.
--
-- Idempotent: re-runnable against any database that has run tidx migrations
-- (e.g. the test DB: `DATABASE_URL=… cargo test` sets it up). Uses block
-- numbers 999001–999003 and 0xfeed… hashes so it can never collide with real
-- Igra data. Run: psql "$DATABASE_URL" -f setup.sql   (or via run_local.py)
BEGIN;

DELETE FROM internal_txs WHERE block_num BETWEEN 999001 AND 999003;
DELETE FROM logs         WHERE block_num BETWEEN 999001 AND 999003;
DELETE FROM receipts     WHERE block_num BETWEEN 999001 AND 999003;
DELETE FROM txs          WHERE block_num BETWEEN 999001 AND 999003;
DELETE FROM blocks       WHERE num       BETWEEN 999001 AND 999003;
DELETE FROM orphaned_logs WHERE reorg_id = 999; DELETE FROM orphaned_receipts WHERE reorg_id = 999;
DELETE FROM orphaned_internal_txs WHERE reorg_id = 999; DELETE FROM orphaned_txs WHERE reorg_id = 999;
DELETE FROM orphaned_blocks WHERE reorg_id = 999; DELETE FROM reorgs WHERE id = 999;

-- Actors. W = the watched wallet in every local case.
--   W  = 0x1111…1111    A = 0xaaaa…aaaa    B = 0xbbbb…bbbb    C = 0xcccc…cccc
--   token T = 0x1717…1717 (a local ERC-20; only its Transfer logs matter)

------------------------------------------------------------------------------
-- Block 999001 — F4 + F4c: reverted subtrees and CREATE2, one SUCCESSFUL tx.
------------------------------------------------------------------------------
INSERT INTO blocks (num, hash, parent_hash, timestamp, timestamp_ms, gas_limit, gas_used, miner) VALUES
 (999001, '\xb10c000000000000000000000000000000000000000000000000000000999001', '\xb10c000000000000000000000000000000000000000000000000000000999000', '2026-09-28T12:00:00+00', 1790000000000, 10000000, 500000, '\x0000000000000000000000000000000000000fee');
INSERT INTO txs (block_num, block_timestamp, idx, hash, type, "from", "to", value, input, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce_key, nonce, selector) VALUES
 (999001, '2026-09-28T12:00:00+00', 0, '\xfeed000000000000000000000000000000000000000000000000000000000001', 2, '\xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', '\xcccccccccccccccccccccccccccccccccccccccc', '0', '\xdeadbeef', 500000, '1', '1', '\x00', 1, '\xdeadbeef');
INSERT INTO receipts (block_num, block_timestamp, tx_idx, tx_hash, "from", "to", gas_used, cumulative_gas_used, status) VALUES
 (999001, '2026-09-28T12:00:00+00', 0, '\xfeed000000000000000000000000000000000000000000000000000000000001', '\xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', '\xcccccccccccccccccccccccccccccccccccccccc', 400000, 400000, 1);
-- DFS-preorder trace. Expected q2 output for WATCHED = W: EXACTLY p5 (7, CALL) and p9 (13, CREATE2).
--   p1 reverted (depth 2) → subtree (1,5): p2 value 5 to W EXCLUDED
--   p6 DELEGATECALL value 9 to W                            EXCLUDED (call_type)
--   p7 reverted (depth 2) → subtree (7,9): p8 value 11 to W EXCLUDED
--   p9 CREATE2 value 13 to W, outside any reverted subtree  INCLUDED
--   p10 reverted (depth 2) → subtree (10,inf): p11 CREATE2 value 17 to W EXCLUDED
INSERT INTO internal_txs (block_num, block_timestamp, tx_idx, tx_hash, depth, path_idx, call_type, "from", "to", value, input, output, gas_used, error) VALUES
 (999001,'2026-09-28T12:00:00+00',0,'\xfeed000000000000000000000000000000000000000000000000000000000001',1, 0,'CALL',        '\xcccccccccccccccccccccccccccccccccccccccc','\xdddddddddddddddddddddddddddddddddddddddd','0', '\x','\x',1000,NULL),
 (999001,'2026-09-28T12:00:00+00',0,'\xfeed000000000000000000000000000000000000000000000000000000000001',2, 1,'CALL',        '\xdddddddddddddddddddddddddddddddddddddddd','\xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee','0', '\x','\x',1000,'execution reverted'),
 (999001,'2026-09-28T12:00:00+00',0,'\xfeed000000000000000000000000000000000000000000000000000000000001',3, 2,'CALL',        '\xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee','\x1111111111111111111111111111111111111111','5', '\x','\x',1000,NULL),
 (999001,'2026-09-28T12:00:00+00',0,'\xfeed000000000000000000000000000000000000000000000000000000000001',4, 3,'STATICCALL',  '\x1111111111111111111111111111111111111111','\xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee','0', '\x','\x',100,NULL),
 (999001,'2026-09-28T12:00:00+00',0,'\xfeed000000000000000000000000000000000000000000000000000000000001',3, 4,'CALL',        '\xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee','\xffffffffffffffffffffffffffffffffffffffff','0', '\x','\x',1000,NULL),
 (999001,'2026-09-28T12:00:00+00',0,'\xfeed000000000000000000000000000000000000000000000000000000000001',2, 5,'CALL',        '\xdddddddddddddddddddddddddddddddddddddddd','\x1111111111111111111111111111111111111111','7', '\x','\x',1000,NULL),
 (999001,'2026-09-28T12:00:00+00',0,'\xfeed000000000000000000000000000000000000000000000000000000000001',2, 6,'DELEGATECALL','\xdddddddddddddddddddddddddddddddddddddddd','\x1111111111111111111111111111111111111111','9', '\x','\x',1000,NULL),
 (999001,'2026-09-28T12:00:00+00',0,'\xfeed000000000000000000000000000000000000000000000000000000000001',2, 7,'CALL',        '\xdddddddddddddddddddddddddddddddddddddddd','\xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee','0', '\x','\x',1000,'execution reverted'),
 (999001,'2026-09-28T12:00:00+00',0,'\xfeed000000000000000000000000000000000000000000000000000000000001',3, 8,'CALL',        '\xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee','\x1111111111111111111111111111111111111111','11','\x','\x',1000,NULL),
 (999001,'2026-09-28T12:00:00+00',0,'\xfeed000000000000000000000000000000000000000000000000000000000001',2, 9,'CREATE2',     '\xdddddddddddddddddddddddddddddddddddddddd','\x1111111111111111111111111111111111111111','13','\x','\x',1000,NULL),
 (999001,'2026-09-28T12:00:00+00',0,'\xfeed000000000000000000000000000000000000000000000000000000000001',2,10,'CALL',        '\xdddddddddddddddddddddddddddddddddddddddd','\xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee','0', '\x','\x',1000,'execution reverted'),
 (999001,'2026-09-28T12:00:00+00',0,'\xfeed000000000000000000000000000000000000000000000000000000000001',3,11,'CREATE2',     '\xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee','\x1111111111111111111111111111111111111111','17','\x','\x',1000,NULL);

------------------------------------------------------------------------------
-- Block 999002 — F1c: seed → recipient → next recipient in the SAME block.
--   tx#0  A → B   500 T   (direct transfer(); B is an EOA)
--   tx#1  B → C   200 T
-- Poll with WATCHED = A over [999002,999002] returns tx#0 (direction out).
-- Recipient rescan with WATCHED = B over the SAME window returns tx#1 — the
-- later transaction in the same block — which the bot would otherwise miss.
------------------------------------------------------------------------------
INSERT INTO blocks (num, hash, parent_hash, timestamp, timestamp_ms, gas_limit, gas_used, miner) VALUES
 (999002, '\xb10c000000000000000000000000000000000000000000000000000000999002', '\xb10c000000000000000000000000000000000000000000000000000000999001', '2026-09-28T12:00:01+00', 1790000001000, 10000000, 100000, '\x0000000000000000000000000000000000000fee');
INSERT INTO txs (block_num, block_timestamp, idx, hash, type, "from", "to", value, input, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce_key, nonce, selector) VALUES
 (999002, '2026-09-28T12:00:01+00', 0, '\xfeed000000000000000000000000000000000000000000000000000000000002', 2, '\xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', '\x1717171717171717171717171717171717171717', '0', '\xa9059cbb', 100000, '1', '1', '\x00', 2, '\xa9059cbb'),
 (999002, '2026-09-28T12:00:01+00', 1, '\xfeed000000000000000000000000000000000000000000000000000000000003', 2, '\xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', '\x1717171717171717171717171717171717171717', '0', '\xa9059cbb', 100000, '1', '1', '\x00', 1, '\xa9059cbb');
INSERT INTO receipts (block_num, block_timestamp, tx_idx, tx_hash, "from", "to", gas_used, cumulative_gas_used, status) VALUES
 (999002, '2026-09-28T12:00:01+00', 0, '\xfeed000000000000000000000000000000000000000000000000000000000002', '\xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', '\x1717171717171717171717171717171717171717', 50000, 50000, 1),
 (999002, '2026-09-28T12:00:01+00', 1, '\xfeed000000000000000000000000000000000000000000000000000000000003', '\xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', '\x1717171717171717171717171717171717171717', 50000, 100000, 1);
INSERT INTO logs (block_num, block_timestamp, log_idx, tx_idx, tx_hash, address, topic0, topic1, topic2, topic3, data) VALUES
 (999002, '2026-09-28T12:00:01+00', 0, 0, '\xfeed000000000000000000000000000000000000000000000000000000000002', '\x1717171717171717171717171717171717171717',
  '\xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef', '\x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', '\x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', NULL,
  '\x00000000000000000000000000000000000000000000000000000000000001f4'),   -- 500
 (999002, '2026-09-28T12:00:01+00', 1, 1, '\xfeed000000000000000000000000000000000000000000000000000000000003', '\x1717171717171717171717171717171717171717',
  '\xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef', '\x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', '\x000000000000000000000000cccccccccccccccccccccccccccccccccccccccc', NULL,
  '\x00000000000000000000000000000000000000000000000000000000000000c8');   -- 200

------------------------------------------------------------------------------
-- Block 999003 — F10b: a reorg that ORPHANED an observed movement.
-- Canonical block 999003 is the replacement (hash …9003); the orphaned twin
-- (hash …dead) held tx feed…04: A → B, 5 wei native AND a 900 T transfer.
-- q1o / q3o with REORG_ID = 999, WATCHED = A must return that tx WITH the
-- orphaned block hash …dead, so the bot can issue a correction keyed on it.
------------------------------------------------------------------------------
INSERT INTO blocks (num, hash, parent_hash, timestamp, timestamp_ms, gas_limit, gas_used, miner) VALUES
 (999003, '\xb10c000000000000000000000000000000000000000000000000000000999003', '\xb10c000000000000000000000000000000000000000000000000000000999002', '2026-09-28T12:00:02+00', 1790000002000, 10000000, 0, '\x0000000000000000000000000000000000000fee');
INSERT INTO reorgs (id, fork_point, prev_tip, depth, blocks_removed, txs_removed, logs_removed, receipts_removed, internal_txs_removed, withdrawals_removed, count_check_ok, occurred_at) OVERRIDING SYSTEM VALUE VALUES
 (999, 999002, 999003, 1, 1, 1, 1, 1, 0, 0, true, '2026-09-28T12:00:03+00');
INSERT INTO orphaned_blocks (reorg_id, num, hash, parent_hash, timestamp, timestamp_ms, gas_limit, gas_used, miner) VALUES
 (999, 999003, '\xdead000000000000000000000000000000000000000000000000000000999003', '\xb10c000000000000000000000000000000000000000000000000000000999002', '2026-09-28T12:00:02+00', 1790000002000, 10000000, 60000, '\x0000000000000000000000000000000000000fee');
INSERT INTO orphaned_txs (reorg_id, block_num, block_timestamp, idx, hash, type, "from", "to", value, input, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce_key, nonce, call_count, selector) VALUES
 (999, 999003, '2026-09-28T12:00:02+00', 0, '\xfeed000000000000000000000000000000000000000000000000000000000004', 2, '\xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', '\xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', '5', '\x', 100000, '1', '1', '\x00', 3, 0, NULL);
INSERT INTO orphaned_receipts (reorg_id, block_num, block_timestamp, tx_idx, tx_hash, "from", "to", gas_used, cumulative_gas_used, status) VALUES
 (999, 999003, '2026-09-28T12:00:02+00', 0, '\xfeed000000000000000000000000000000000000000000000000000000000004', '\xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', '\xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 60000, 60000, 1);
INSERT INTO orphaned_logs (reorg_id, block_num, block_timestamp, log_idx, tx_idx, tx_hash, address, topic0, topic1, topic2, topic3, data) VALUES
 (999, 999003, '2026-09-28T12:00:02+00', 0, 0, '\xfeed000000000000000000000000000000000000000000000000000000000004', '\x1717171717171717171717171717171717171717',
  '\xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef', '\x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', '\x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', NULL,
  '\x0000000000000000000000000000000000000000000000000000000000000384');   -- 900

COMMIT;
