-- Telegram: which entities each bot message was about.
--
-- Additive only. When Alex replies to a bot message ("erinnere sie daran",
-- "ist erledigt"), the message text only names a customer — "Neue Anfrage von
-- Frederike" — and Josie had to search by name, which picked the wrong person
-- when two customers share one (the Frederike mis-reminder). Every bot message
-- now records the IDs behind it here, keyed by the Telegram message ID, and a
-- reply resolves its referent from this table instead of from the wording.
-- `refs` is a flat JSON object of *_id keys plus an optional "ids" array.

CREATE TABLE telegram_message_refs (
    tenant_id   UUID        NOT NULL DEFAULT current_tenant_id() REFERENCES tenants(id),
    chat_id     BIGINT      NOT NULL,
    message_id  BIGINT      NOT NULL,
    refs        JSONB       NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, chat_id, message_id)
);

CREATE INDEX idx_telegram_message_refs_created ON telegram_message_refs (created_at);

ALTER TABLE telegram_message_refs ENABLE ROW LEVEL SECURITY;
ALTER TABLE telegram_message_refs FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON telegram_message_refs
    USING (tenant_id = current_tenant_id() OR tenant_bypass())
    WITH CHECK (tenant_id = current_tenant_id() OR tenant_bypass());

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'aust_assistant') THEN
        GRANT SELECT, INSERT, UPDATE, DELETE ON telegram_message_refs TO aust_assistant;
    END IF;
END $$;
