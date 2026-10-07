-- Multi-tenancy, step 4: every company's own document templates.
--
-- Additive only. Aust has no rows here — its templates stay compiled in
-- (templates/*). Any other company's offers, invoices and forms are built on the
-- template it stores here; without one, generating fails instead of printing
-- Aust's letterhead. See crates/offer-generator/src/templates.rs.

CREATE TABLE tenant_templates (
    tenant_id   UUID NOT NULL DEFAULT current_tenant_id() REFERENCES tenants(id),
    kind        TEXT NOT NULL CHECK (kind IN ('offer', 'invoice', 'travel_expense', 'clearing_page_2')),
    content     BYTEA NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, kind)
);

ALTER TABLE tenant_templates ENABLE ROW LEVEL SECURITY;
ALTER TABLE tenant_templates FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON tenant_templates
    USING (tenant_id = current_tenant_id() OR tenant_bypass())
    WITH CHECK (tenant_id = current_tenant_id() OR tenant_bypass());
