-- Neutralize the aust_assistant DB role created by migration 20260609000008.
-- The role was never assumed at runtime (the API uses a single pool with API
-- privileges), so its privilege grants were misleading documentation.
--
-- We cannot DROP the role here because grants exist across multiple databases
-- (dev + test) and a single-DB migration only revokes within its own DB. So we
-- REVOKE everything in this DB and leave the role itself (harmless once stripped
-- of privileges). If you ever want to fully drop it: connect to every database
-- where the migration ran, then `DROP ROLE aust_assistant` from a superuser session.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'aust_assistant') THEN
        EXECUTE 'REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA public FROM aust_assistant';
        EXECUTE 'REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public FROM aust_assistant';
        EXECUTE 'REVOKE ALL PRIVILEGES ON ALL FUNCTIONS IN SCHEMA public FROM aust_assistant';
        EXECUTE 'REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA public FROM aust_assistant';
        EXECUTE 'REVOKE ALL PRIVILEGES ON SCHEMA public FROM aust_assistant';
        EXECUTE 'REVOKE ALL PRIVILEGES ON DATABASE ' || quote_ident(current_database()) || ' FROM aust_assistant';
    END IF;
END$$;
