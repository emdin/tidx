-- Rollback for db/reorg_archive.sql.
-- NOT auto-run. Execute manually to remove the archive tables:
--   docker compose -f deploy/prod/docker-compose.yml exec -T postgres \
--     psql -U tidx -d tidx_igra -f /dev/stdin < db/undo_reorg_archive.sql
--
-- CAUTION: drops archived rows. Take a snapshot first if you want them:
--   pg_dump -U tidx -d tidx_igra -t 'reorgs' -t 'orphaned_*' > reorg-archive-snapshot.sql

BEGIN;

DROP TABLE IF EXISTS orphaned_blocks         CASCADE;
DROP TABLE IF EXISTS orphaned_l2_withdrawals CASCADE;
DROP TABLE IF EXISTS orphaned_internal_txs   CASCADE;
DROP TABLE IF EXISTS orphaned_receipts       CASCADE;
DROP TABLE IF EXISTS orphaned_logs           CASCADE;
DROP TABLE IF EXISTS orphaned_txs            CASCADE;
DROP TABLE IF EXISTS reorgs                  CASCADE;

COMMIT;
