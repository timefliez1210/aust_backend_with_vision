-- Multi-tenancy, step 1: every row belongs to a tenant.
--
-- Additive only. Aust Umzüge becomes tenant #1 and owns every existing row; nothing
-- reads tenant_id yet, so behaviour is unchanged. See docs/MULTI_TENANT.md.
--
-- The application tells Postgres which tenant a connection works for via the
-- session setting `app.tenant_id` (set on every pool acquire, see
-- crates/api/src/tenant.rs). `current_tenant_id()` reads it and, while Aust is the
-- only tenant, falls back to Aust when it is unset — so background jobs, tests and
-- psql sessions keep working. Before a second tenant goes live this fallback is
-- replaced by an error (one function, one switch).

CREATE TABLE tenants (
    id          UUID PRIMARY KEY,
    slug        TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO tenants (id, slug, name)
VALUES ('0190aa57-0000-7000-8000-000000000001', 'aust', 'Aust Umzüge & Haushaltsauflösungen');

CREATE FUNCTION current_tenant_id() RETURNS UUID
LANGUAGE sql STABLE AS $$
    SELECT COALESCE(
        NULLIF(current_setting('app.tenant_id', true), '')::uuid,
        '0190aa57-0000-7000-8000-000000000001'::uuid
    )
$$;

-- STABLE default → Postgres evaluates it once for existing rows (no rewrite) and
-- per insert afterwards, so new rows land in the connection's tenant without any
-- INSERT statement having to name the column.
DO $$
DECLARE
    t TEXT;
BEGIN
    FOR t IN
        SELECT table_name
        FROM information_schema.tables
        WHERE table_schema = 'public'
          AND table_type = 'BASE TABLE'
          AND table_name NOT IN ('_sqlx_migrations', 'tenants')
    LOOP
        EXECUTE format(
            'ALTER TABLE %I ADD COLUMN tenant_id UUID NOT NULL DEFAULT current_tenant_id() REFERENCES tenants(id)',
            t
        );
    END LOOP;
END
$$;
