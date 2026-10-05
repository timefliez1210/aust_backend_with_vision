-- Multi-tenancy, step 5: "unique" means unique within one company.
--
-- NOT additive: drops seven single-column unique keys and replaces each with the
-- same key per tenant (approved exception to "migrations additive-only",
-- 2026-10-05). With one tenant nothing changes in what is allowed.
-- See docs/MULTI_TENANT.md.

-- ── customers.email ──────────────────────────────────────────────────────────
-- The old key let two exact duplicates through on prod (corrupt index, see the
-- 2026-09-27 ops findings): t.finkbeiner@erfi.de and a MAILER-DAEMON address
-- each exist twice. Merge every exact duplicate into its oldest row exactly as
-- the app's own customer merge does (re-point inquiries, sessions, threads and
-- assistant memory; mark the newer row `merged_into`), so the new key can be
-- built. Merged rows keep their email and are left out of the key, as the app's
-- merge already intends.
DO $$
DECLARE
    d RECORD;
BEGIN
    FOR d IN
        SELECT c.id AS merge_id, k.id AS keep_id
        FROM customers c
        JOIN LATERAL (
            SELECT k.id FROM customers k
            WHERE k.tenant_id = c.tenant_id AND k.email = c.email AND k.merged_into IS NULL
            ORDER BY k.created_at, k.id
            LIMIT 1
        ) k ON k.id <> c.id
        WHERE c.email IS NOT NULL AND c.merged_into IS NULL
    LOOP
        UPDATE inquiries SET customer_id = d.keep_id WHERE customer_id = d.merge_id;
        UPDATE customer_sessions SET customer_id = d.keep_id WHERE customer_id = d.merge_id;
        UPDATE email_threads SET customer_id = d.keep_id WHERE customer_id = d.merge_id;
        UPDATE agent_memory SET scope = 'customer:' || d.keep_id
            WHERE scope = 'customer:' || d.merge_id;
        UPDATE agent_episodes
            SET refs = refs - 'customer_id' || jsonb_build_object('customer_id', d.keep_id::text)
            WHERE refs->>'customer_id' = d.merge_id::text;
        UPDATE customers
            SET merged_into = d.keep_id,
                notes = COALESCE(notes, '') || ' [MERGED INTO ' || d.keep_id::text || ']'
            WHERE id = d.merge_id;
    END LOOP;
END
$$;

-- IF EXISTS: a restored backup cannot rebuild the old key (the duplicates above).
ALTER TABLE customers DROP CONSTRAINT IF EXISTS customers_email_key;
CREATE UNIQUE INDEX customers_tenant_email_key
    ON customers (tenant_id, email) WHERE merged_into IS NULL;

-- ── Numbers and names that are per company ───────────────────────────────────
ALTER TABLE invoices DROP CONSTRAINT invoices_invoice_number_key;
ALTER TABLE invoices ADD CONSTRAINT invoices_tenant_invoice_number_key
    UNIQUE (tenant_id, invoice_number);

ALTER TABLE storage_invoices DROP CONSTRAINT storage_invoices_invoice_number_key;
ALTER TABLE storage_invoices ADD CONSTRAINT storage_invoices_tenant_invoice_number_key
    UNIQUE (tenant_id, invoice_number);

ALTER TABLE expense_categories DROP CONSTRAINT expense_categories_name_key;
ALTER TABLE expense_categories ADD CONSTRAINT expense_categories_tenant_name_key
    UNIQUE (tenant_id, name);

ALTER TABLE calendar_capacity_overrides
    DROP CONSTRAINT calendar_capacity_overrides_override_date_key;
ALTER TABLE calendar_capacity_overrides
    ADD CONSTRAINT calendar_capacity_overrides_tenant_date_key UNIQUE (tenant_id, override_date);

ALTER TABLE settings DROP CONSTRAINT settings_pkey;
ALTER TABLE settings ADD PRIMARY KEY (tenant_id, key);

ALTER TABLE invoice_number_counters DROP CONSTRAINT invoice_number_counters_pkey;
ALTER TABLE invoice_number_counters ADD PRIMARY KEY (tenant_id, year);

-- ── KVA numbers ──────────────────────────────────────────────────────────────
-- Aust keeps drawing from `offer_number_seq`, so its numbering never jumps. Every
-- other company counts in its own row here.
CREATE TABLE offer_number_counters (
    tenant_id   UUID PRIMARY KEY DEFAULT current_tenant_id() REFERENCES tenants(id),
    last_value  BIGINT NOT NULL
);
ALTER TABLE offer_number_counters ENABLE ROW LEVEL SECURITY;
ALTER TABLE offer_number_counters FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON offer_number_counters
    USING (tenant_id = current_tenant_id() OR tenant_bypass())
    WITH CHECK (tenant_id = current_tenant_id() OR tenant_bypass());
