-- Telegram: notifications the office has switched off.
--
-- Additive only. One row per company and notification kind that is muted; no row
-- means the notification is sent. Every informational Telegram message carries a
-- "🔕 Stumm schalten" button that inserts the row, and /benachrichtigungen lists
-- all kinds to switch them back on. Kinds are defined in
-- crates/core/src/notifications.rs; no CHECK here so a new kind needs no migration.

CREATE TABLE telegram_muted_notifications (
    tenant_id   UUID NOT NULL DEFAULT current_tenant_id() REFERENCES tenants(id),
    kind        TEXT NOT NULL,
    muted_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, kind)
);

ALTER TABLE telegram_muted_notifications ENABLE ROW LEVEL SECURITY;
ALTER TABLE telegram_muted_notifications FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON telegram_muted_notifications
    USING (tenant_id = current_tenant_id() OR tenant_bypass())
    WITH CHECK (tenant_id = current_tenant_id() OR tenant_bypass());

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'aust_assistant') THEN
        GRANT SELECT, INSERT, DELETE ON telegram_muted_notifications TO aust_assistant;
    END IF;
END $$;
