-- One-time setup for `tidx watch-tip`. Run it as the superuser on the tidx
-- database, then give tidx_tipwatch a password:
--
--   psql -U tidx -d tidx_igra -f db/tipwatch_role.sql
--   psql -U tidx -d tidx_igra    then:  \password tidx_tipwatch
--
-- Use \password or SET log_statement = 'none' first (see app_role.sql).
--
-- The watcher talks to public RPC endpoints all day, so it gets its own login
-- role that owns the tipwatch schema and nothing else: it cannot read or change
-- the indexed tables. Idempotent.

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'tidx_tipwatch') THEN
        CREATE ROLE tidx_tipwatch LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT;
    END IF;
END
$$;

CREATE SCHEMA IF NOT EXISTS tipwatch AUTHORIZATION tidx_tipwatch;
ALTER SCHEMA tipwatch OWNER TO tidx_tipwatch;
