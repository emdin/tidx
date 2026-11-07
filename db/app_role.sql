-- One-time setup so the sync engine runs as an ordinary role, not the
-- bootstrap superuser. Run it as the superuser (POSTGRES_USER of the Postgres
-- container) on the tidx database, then give tidx_app a password and point
-- pg_url at it:
--
--   psql -U tidx -d tidx_igra -f db/app_role.sql
--   psql -U tidx -d tidx_igra    then:  \password tidx_app
--
-- Use \password (it sends a hash) or SET log_statement = 'none' in the same
-- session: with log_statement=ddl a plain ALTER ROLE ... PASSWORD '...' lands
-- in the Postgres log in clear text.
--
-- 2026-11-06: tidx connected as the bootstrap superuser, so a /query validator
-- gap became pg_read_file, lo_export and code execution in the container.
-- tidx_app owns the schema and nothing more: no superuser, CREATEROLE,
-- CREATEDB, replication, or file access. Idempotent; run it again after
-- objects were created by the superuser and it hands them over.
--
-- Changing owners takes a brief exclusive lock per table, so stop the engine
-- first on a live database.

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'tidx_app') THEN
        CREATE ROLE tidx_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
    END IF;
    -- Created here so query_role.sql never needs CREATEROLE when tidx_app runs it.
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'tidx_query') THEN
        CREATE ROLE tidx_query NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOINHERIT;
    END IF;
    EXECUTE format('GRANT CONNECT, TEMPORARY ON DATABASE %I TO tidx_app', current_database());
END $$;

-- Enforced on every run, not only at creation: a role left over with extra
-- powers (an earlier manual attempt, or an intruder with superuser) must not
-- keep them.
ALTER ROLE tidx_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE tidx_query NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT;

-- No role memberships besides tidx_app -> tidx_query (e.g. not
-- pg_read_server_files or pg_execute_server_program).
DO $$
DECLARE
    r record;
BEGIN
    FOR r IN
        SELECT m.rolname AS member, g.rolname AS granted, gr.rolname AS grantor
        FROM pg_auth_members am
        JOIN pg_roles m ON m.oid = am.member
        JOIN pg_roles g ON g.oid = am.roleid
        JOIN pg_roles gr ON gr.oid = am.grantor
        WHERE m.rolname IN ('tidx_app', 'tidx_query')
          AND NOT (m.rolname = 'tidx_app' AND g.rolname = 'tidx_query')
    LOOP
        EXECUTE format('REVOKE %I FROM %I GRANTED BY %I CASCADE', r.granted, r.member, r.grantor);
    END LOOP;
END $$;

-- The public /query transaction runs SET LOCAL ROLE tidx_query.
GRANT tidx_query TO tidx_app;
-- WITH GRANT OPTION so query_role.sql's GRANT USAGE to tidx_query on every
-- boot does not log "no privileges were granted".
GRANT USAGE ON SCHEMA public TO tidx_app WITH GRANT OPTION;
GRANT CREATE ON SCHEMA public TO tidx_app;
GRANT USAGE ON SCHEMA public TO tidx_query;

-- Hand every object in public to tidx_app, so its migrations (ALTER TABLE,
-- CREATE INDEX, CREATE OR REPLACE FUNCTION, GRANT to tidx_query) keep working.
DO $$
DECLARE
    r record;
BEGIN
    FOR r IN
        SELECT c.oid::regclass AS obj,
               CASE c.relkind WHEN 'v' THEN 'VIEW' WHEN 'm' THEN 'MATERIALIZED VIEW'
                              WHEN 'f' THEN 'FOREIGN TABLE' ELSE 'TABLE' END AS kind
        FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
          AND c.relowner <> 'tidx_app'::regrole
    LOOP
        EXECUTE format('ALTER %s %s OWNER TO tidx_app', r.kind, r.obj);
    END LOOP;

    -- Standalone sequences only: one owned by a column (serial, identity)
    -- moves with its table and cannot be re-owned on its own.
    FOR r IN
        SELECT c.oid::regclass AS obj
        FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'public' AND c.relkind = 'S'
          AND c.relowner <> 'tidx_app'::regrole
          AND NOT EXISTS (
              SELECT 1 FROM pg_depend d
              WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid
                AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i')
          )
    LOOP
        EXECUTE format('ALTER SEQUENCE %s OWNER TO tidx_app', r.obj);
    END LOOP;

    FOR r IN
        SELECT p.oid::regprocedure AS obj,
               CASE p.prokind WHEN 'p' THEN 'PROCEDURE' WHEN 'a' THEN 'AGGREGATE'
                              ELSE 'FUNCTION' END AS kind
        FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
        WHERE n.nspname = 'public' AND p.proowner <> 'tidx_app'::regrole
    LOOP
        EXECUTE format('ALTER %s %s OWNER TO tidx_app', r.kind, r.obj);
    END LOOP;

    -- Standalone types (enums, domains, composites); table row types follow their table.
    FOR r IN
        SELECT t.oid::regtype AS obj,
               CASE t.typtype WHEN 'd' THEN 'DOMAIN' ELSE 'TYPE' END AS kind
        FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
        WHERE n.nspname = 'public' AND t.typtype IN ('e', 'd', 'c', 'r', 'm')
          AND (t.typrelid = 0 OR (SELECT relkind FROM pg_class WHERE oid = t.typrelid) = 'c')
          AND t.typowner <> 'tidx_app'::regrole
    LOOP
        EXECUTE format('ALTER %s %s OWNER TO tidx_app', r.kind, r.obj);
    END LOOP;
END $$;
