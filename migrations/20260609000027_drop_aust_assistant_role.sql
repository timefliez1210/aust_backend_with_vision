-- Neutralize the aust_assistant DB role created by migration 20260609000008.
-- The role was never assumed at runtime (the API uses a single pool with API
-- privileges), so its privilege grants were misleading documentation.
--
-- ACCEPTED RISK (M4, 2026-05-29): the assistant process runs against the
-- single API pool, which has full CRUD on every business table plus read on
-- users/customer_otps/customer_sessions. The assistant ingests LLM-shaped
-- output derived from attacker-controllable email/inquiry content, so a
-- successful prompt-injection could in principle reach those tables. The
-- documented privacy boundary is therefore aspirational, not enforced.
--
-- Mitigations in place today: (a) Safety::Confirm on every customer-facing
-- and destructive tool, (b) `remember` is Confirm so durable rules can't be
-- planted silently (B6), (c) `pending_actions.chat_id` ownership check on
-- resolve (S2/H3), (d) audit log of every tool call.
--
-- Promote to enforced boundary by: opening a second sqlx pool that
-- `SET ROLE aust_assistant`, plumbing it into ServiceBundle constructors,
-- restoring the grants in 20260609000008, and removing this revoke.
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
