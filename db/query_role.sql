-- Role the public /query endpoint runs as (SET LOCAL ROLE inside its
-- transaction). 2026-11-06: /query ran as the bootstrap superuser, and a
-- validator gap let pg_read_file / lo_export plant a library that gave code
-- execution in the Postgres container. This role can only read the public schema.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'tidx_query') THEN
        CREATE ROLE tidx_query NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOINHERIT;
    END IF;
END $$;

GRANT USAGE ON SCHEMA public TO tidx_query;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO tidx_query;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO tidx_query;
