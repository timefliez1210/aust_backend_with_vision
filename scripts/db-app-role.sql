-- One-time: let the backend connect as a non-superuser that owns the schema, so
-- row-level security (docs/MULTI_TENANT.md, step 3) is enforced.
--
-- Run as the superuser, against the app database:
--   docker exec -i aust_postgres psql -U aust -d aust_backend \
--     -v app_password="'…'" < scripts/db-app-role.sql
-- then point the backend's database URL at aust_app and restart it.
-- Rollback: point it back at the superuser — superusers skip row-level security.
--
-- Idempotent: safe to run again.

\set ON_ERROR_STOP on

DO $$
BEGIN
    -- Migrations grant to the assistant's group role; a fresh cluster may lack it.
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'aust_assistant') THEN
        CREATE ROLE aust_assistant NOLOGIN;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'aust_app') THEN
        CREATE ROLE aust_app LOGIN NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE;
    END IF;
END
$$;
ALTER ROLE aust_app PASSWORD :app_password;

-- Migrations grant to the assistant's group role; the new owner must be able to.
GRANT aust_assistant TO aust_app WITH ADMIN OPTION;

DO $$
DECLARE
    r RECORD;
BEGIN
    EXECUTE format('GRANT CONNECT, CREATE ON DATABASE %I TO aust_app', current_database());
    EXECUTE 'ALTER SCHEMA public OWNER TO aust_app';

    -- Tables (incl. _sqlx_migrations), sequences, views.
    FOR r IN
        SELECT c.relname, c.relkind
        FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p', 'S', 'v', 'm')
    LOOP
        EXECUTE format(
            'ALTER %s %I OWNER TO aust_app',
            CASE r.relkind WHEN 'S' THEN 'SEQUENCE' WHEN 'v' THEN 'VIEW'
                           WHEN 'm' THEN 'MATERIALIZED VIEW' ELSE 'TABLE' END,
            r.relname
        );
    END LOOP;

    -- Functions defined by migrations (not those of extensions).
    FOR r IN
        SELECT p.oid::regprocedure AS sig
        FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
        WHERE n.nspname = 'public'
          AND NOT EXISTS (SELECT 1 FROM pg_depend d
                          WHERE d.objid = p.oid AND d.deptype = 'e')
    LOOP
        EXECUTE format('ALTER FUNCTION %s OWNER TO aust_app', r.sig);
    END LOOP;

    -- Enum and other user types.
    FOR r IN
        SELECT t.typname
        FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
        WHERE n.nspname = 'public' AND t.typtype IN ('e', 'd')
          AND NOT EXISTS (SELECT 1 FROM pg_depend d
                          WHERE d.objid = t.oid AND d.deptype = 'e')
    LOOP
        EXECUTE format('ALTER TYPE %I OWNER TO aust_app', r.typname);
    END LOOP;
END
$$;

-- Check: nothing in public is left with another owner, and the app role is plain.
SELECT 'not owned by aust_app: ' || c.relname
FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p', 'S', 'v', 'm')
  AND pg_get_userbyid(c.relowner) <> 'aust_app';
SELECT rolname, rolsuper, rolbypassrls FROM pg_roles WHERE rolname = 'aust_app';
