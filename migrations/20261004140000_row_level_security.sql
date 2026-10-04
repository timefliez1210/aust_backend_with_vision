-- Multi-tenancy, step 3: Postgres itself keeps companies apart.
--
-- Additive only. Every tenant table gets row-level security with one policy:
-- a row is visible and writable when it belongs to `current_tenant_id()`, or
-- when the transaction deliberately opened a bypass (`app.tenant_bypass = 'on'`,
-- see aust_core::tenant::bypass) — for the few lookups that run before the
-- tenant is known: login by email, session tokens.
--
-- Invisible until the app connects as a non-superuser role: superusers skip
-- row-level security even when it is FORCEd. See docs/MULTI_TENANT.md for the
-- role switch.

CREATE FUNCTION tenant_bypass() RETURNS BOOLEAN
LANGUAGE sql STABLE AS $$
    SELECT COALESCE(current_setting('app.tenant_bypass', true), '') = 'on'
$$;

DO $$
DECLARE
    t TEXT;
BEGIN
    FOR t IN
        SELECT table_name
        FROM information_schema.columns
        WHERE table_schema = 'public' AND column_name = 'tenant_id'
          AND table_name IN (
              SELECT table_name FROM information_schema.tables
              WHERE table_schema = 'public' AND table_type = 'BASE TABLE'
          )
    LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %I
                 USING (tenant_id = current_tenant_id() OR tenant_bypass())
                 WITH CHECK (tenant_id = current_tenant_id() OR tenant_bypass())',
            t
        );
    END LOOP;
END
$$;
