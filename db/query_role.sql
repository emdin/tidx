-- Role the public /query endpoint runs as (SET LOCAL ROLE inside its
-- transaction). 2026-11-06: /query ran as the bootstrap superuser, and a
-- validator gap let pg_read_file / lo_export plant a library that gave code
-- execution in the Postgres container. This role can only read the tables the
-- validator allows (ALLOWED_TABLES in src/query/validator.rs); keep both lists
-- in sync.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'tidx_query') THEN
        CREATE ROLE tidx_query NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOINHERIT;
    END IF;
END $$;

REVOKE ALL ON ALL TABLES IN SCHEMA public FROM tidx_query;
GRANT USAGE ON SCHEMA public TO tidx_query;
GRANT SELECT ON
    blocks, txs, logs, receipts, l2_withdrawals, internal_txs, trace_outcomes,
    kaspa_provenance_meta, kaspa_sync_state, kaspa_provenance_gaps,
    kaspa_pending_l2_submissions, kaspa_pending_entries, kaspa_l2_submissions,
    kaspa_entries, reorgs, orphaned_blocks, orphaned_txs, orphaned_logs,
    orphaned_receipts, orphaned_internal_txs, orphaned_l2_withdrawals
TO tidx_query;
