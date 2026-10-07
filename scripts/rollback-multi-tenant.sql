-- Roll the database back to the shape before multi-tenancy, so the previous
-- backend image (before migration 20261004120000) runs on it again.
--
-- Keeps every row written since the deploy. Drops what only the new code uses:
-- tenant_id columns, row-level security, tenants and their profile / templates /
-- letterhead, the superuser flag, per-company counters. Restores the
-- single-column unique keys the old code's ON CONFLICT clauses name.
-- The merge of the two duplicate customers stays (the old code handles merged
-- customers); a merged row whose email a live customer also has loses that email,
-- otherwise the old unique key on customers.email cannot be rebuilt.
--
-- Usage (backend stopped, as the superuser):
--   docker exec -i aust_postgres psql -U aust -d aust_backend -v ON_ERROR_STOP=1 \
--     < scripts/rollback-multi-tenant.sql
-- then start the previous image. Refuses if a second company has any data.
-- Afterwards a redeploy of the new image re-applies all migrations cleanly.

\set ON_ERROR_STOP on
BEGIN;

-- Only one company may exist: rolling back would merge companies into one.
DO $$
BEGIN
    IF (SELECT count(*) FROM tenants) > 1 THEN
        RAISE EXCEPTION 'Rollback refused: more than one company exists';
    END IF;
END
$$;

-- New tables.
DROP TABLE IF EXISTS tenant_templates;
DROP TABLE IF EXISTS offer_number_counters;

-- Row-level security.
DO $$
DECLARE
    t TEXT;
BEGIN
    FOR t IN
        SELECT c.relname FROM pg_class c
        WHERE c.relnamespace = 'public'::regnamespace AND c.relkind = 'r'
          AND (c.relrowsecurity OR c.relforcerowsecurity)
    LOOP
        EXECUTE format('DROP POLICY IF EXISTS tenant_isolation ON %I', t);
        EXECUTE format('ALTER TABLE %I NO FORCE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I DISABLE ROW LEVEL SECURITY', t);
    END LOOP;
END
$$;

-- tenant_id everywhere. CASCADE also drops what was built on it: the per-company
-- keys and primary keys, the same-company link keys and their indexes.
DO $$
DECLARE
    t TEXT;
BEGIN
    FOR t IN
        SELECT table_name FROM information_schema.columns
        WHERE table_schema = 'public' AND column_name = 'tenant_id'
          AND table_name <> 'tenants'
          AND table_name IN (SELECT table_name FROM information_schema.tables
                             WHERE table_schema = 'public' AND table_type = 'BASE TABLE')
    LOOP
        EXECUTE format('ALTER TABLE %I DROP COLUMN tenant_id CASCADE', t);
    END LOOP;
END
$$;

-- The old single-column keys.
UPDATE customers m SET email = NULL
WHERE m.merged_into IS NOT NULL
  AND EXISTS (SELECT 1 FROM customers k
              WHERE k.email = m.email AND k.id <> m.id AND k.merged_into IS NULL);
ALTER TABLE customers ADD CONSTRAINT customers_email_key UNIQUE (email);
ALTER TABLE invoices ADD CONSTRAINT invoices_invoice_number_key UNIQUE (invoice_number);
ALTER TABLE storage_invoices
    ADD CONSTRAINT storage_invoices_invoice_number_key UNIQUE (invoice_number);
ALTER TABLE expense_categories ADD CONSTRAINT expense_categories_name_key UNIQUE (name);
ALTER TABLE calendar_capacity_overrides
    ADD CONSTRAINT calendar_capacity_overrides_override_date_key UNIQUE (override_date);
ALTER TABLE settings ADD CONSTRAINT settings_pkey PRIMARY KEY (key);
ALTER TABLE invoice_number_counters ADD CONSTRAINT invoice_number_counters_pkey PRIMARY KEY (year);
ALTER TABLE agent_briefing_log ADD CONSTRAINT agent_briefing_log_pkey PRIMARY KEY (slot_date, slot);
ALTER TABLE agent_sessions ADD CONSTRAINT agent_sessions_chat_id_key UNIQUE (chat_id);
ALTER TABLE telegram_chat_bindings
    ADD CONSTRAINT telegram_chat_bindings_chat_id_key UNIQUE (chat_id);

-- What remains of the new schema.
ALTER TABLE users DROP COLUMN IF EXISTS is_superuser;
DROP TABLE tenants;
DROP FUNCTION IF EXISTS tenant_bypass();
DROP FUNCTION IF EXISTS current_tenant_id();

-- Forget the migrations, so a later redeploy applies them again.
DELETE FROM _sqlx_migrations WHERE version >= 20261004120000;

COMMIT;
