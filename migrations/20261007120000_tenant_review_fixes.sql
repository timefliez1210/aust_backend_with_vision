-- Multi-tenancy review fixes (docs/MULTI_TENANT.md).
--
-- Not purely additive: three single-company keys become per-company keys, the same
-- exception as 20261005140000_per_tenant_uniqueness. With one company nothing
-- changes in what is allowed.

-- ── Daily briefing: one slot per company and day ───────────────────────────────
-- (slot_date, slot) alone let the first company claim every slot; the others'
-- ON CONFLICT DO NOTHING then silently skipped their briefing.
ALTER TABLE agent_briefing_log DROP CONSTRAINT agent_briefing_log_pkey;
ALTER TABLE agent_briefing_log ADD PRIMARY KEY (tenant_id, slot_date, slot);

-- ── Telegram chats: one per company ──────────────────────────────────────────
-- A private chat has the same id with every bot, so one person talking to two
-- companies' bots needs one session and one binding per company.
ALTER TABLE agent_sessions DROP CONSTRAINT agent_sessions_chat_id_key;
ALTER TABLE agent_sessions ADD CONSTRAINT agent_sessions_tenant_chat_key UNIQUE (tenant_id, chat_id);
ALTER TABLE telegram_chat_bindings DROP CONSTRAINT telegram_chat_bindings_chat_id_key;
ALTER TABLE telegram_chat_bindings ADD CONSTRAINT telegram_chat_bindings_tenant_chat_key UNIQUE (tenant_id, chat_id);

-- ── Links never cross companies ──────────────────────────────────────────────
-- Foreign-key checks ignore row-level security, so a row could point at another
-- company's row (and be cascade-deleted with it). For every single-column
-- foreign key between two tenant tables onto the parent's `id`, add a twin on
-- (tenant_id, column) → parent (tenant_id, id): a link must stay inside one
-- company. NO ACTION is checked at the end of each statement, so the original
-- key's CASCADE / SET NULL still runs first and the twin never blocks it;
-- MATCH SIMPLE skips rows whose column is NULL.
DO $$
DECLARE
    fk RECORD;
    twin TEXT;
BEGIN
    FOR fk IN
        SELECT c.conname,
               c.conrelid::regclass  AS child,
               c.confrelid::regclass AS parent,
               ca.attname            AS col
        FROM pg_constraint c
        JOIN pg_attribute ca ON ca.attrelid = c.conrelid  AND ca.attnum = c.conkey[1]
        JOIN pg_attribute pa ON pa.attrelid = c.confrelid AND pa.attnum = c.confkey[1]
        WHERE c.contype = 'f'
          AND c.connamespace = 'public'::regnamespace
          AND array_length(c.conkey, 1) = 1
          AND pa.attname = 'id'
          AND ca.attname <> 'tenant_id'
          AND EXISTS (SELECT 1 FROM pg_attribute t WHERE t.attrelid = c.conrelid
                      AND t.attname = 'tenant_id' AND NOT t.attisdropped)
          AND EXISTS (SELECT 1 FROM pg_attribute t WHERE t.attrelid = c.confrelid
                      AND t.attname = 'tenant_id' AND NOT t.attisdropped)
    LOOP
        EXECUTE format(
            'CREATE UNIQUE INDEX IF NOT EXISTS %I ON %s (tenant_id, id)',
            left(replace(fk.parent::text, '"', '') || '_tenant_id_id_key', 63),
            fk.parent
        );
        twin := left(fk.conname || '_same_tenant', 63);
        EXECUTE format(
            'ALTER TABLE %s ADD CONSTRAINT %I FOREIGN KEY (tenant_id, %I) REFERENCES %s (tenant_id, id)',
            fk.child, twin, fk.col, fk.parent
        );
    END LOOP;
END
$$;
