-- Active reminders the assistant fires back to Telegram.
--
-- Unlike agent_todos (date-only, passive, session-scoped), a reminder has a
-- precise due_at timestamp and is pushed to a chat by the background reminder
-- tick in src/main.rs. Two flavours:
--   recurrence='none'      → one-shot, deactivated after it fires
--   recurrence='recurring' → re-fires every ~3h within 07:00–20:00 Europe/Berlin
--                            until cancelled (the "permanent nag")
--
-- source/source_ref let the reconciler tie an auto-created reminder to the
-- thing it nags about (currently source='email' → email_messages.id) so it can
-- be auto-cancelled when that item is handled, no matter where it was handled.
CREATE TABLE IF NOT EXISTS agent_reminders (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    chat_id       BIGINT      NOT NULL,
    text          TEXT        NOT NULL,
    due_at        TIMESTAMPTZ NOT NULL,
    recurrence    TEXT        NOT NULL DEFAULT 'none'
                              CHECK (recurrence IN ('none', 'recurring')),
    source        TEXT        NOT NULL DEFAULT 'manual'
                              CHECK (source IN ('manual', 'email')),
    source_ref    UUID,
    active        BOOLEAN     NOT NULL DEFAULT TRUE,
    last_fired_at TIMESTAMPTZ,
    fired_count   INTEGER     NOT NULL DEFAULT 0,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Fast scan for the fire loop: active reminders that are due.
CREATE INDEX IF NOT EXISTS idx_agent_reminders_due
    ON agent_reminders (due_at) WHERE active;

-- At most one active reminder per source item, so the reconciler can insert
-- idempotently (no duplicate nags for the same email).
CREATE UNIQUE INDEX IF NOT EXISTS uq_agent_reminders_active_source_ref
    ON agent_reminders (source, source_ref)
    WHERE active AND source_ref IS NOT NULL;
