-- Per-slot delivery log for the automated daily briefing.
--
-- Alex asked for the daily briefing to be posted automatically at fixed times
-- (07:00 and 15:00 Europe/Berlin) instead of relying on the ~3h recurring
-- reminder, which never lands punctually (feedback report 68ff999e).
--
-- The briefing tick in src/main.rs runs every 60s. To fire each slot exactly
-- once per day — and survive restarts without double-sending or missing — it
-- CLAIMS a slot by inserting the (slot_date, slot) row with ON CONFLICT DO
-- NOTHING. Only the tick that actually inserts the row sends the briefing; a
-- failed send deletes its claim so the next tick retries.
CREATE TABLE IF NOT EXISTS agent_briefing_log (
    slot_date DATE        NOT NULL,
    slot      TEXT        NOT NULL CHECK (slot IN ('morning', 'afternoon')),
    chat_id   BIGINT      NOT NULL,
    sent_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (slot_date, slot)
);

-- The background tick uses the main app pool, but grant to aust_assistant too
-- for parity with the other agent_* tables in case a tool ever reads it.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'aust_assistant') THEN
        GRANT SELECT, INSERT, DELETE ON agent_briefing_log TO aust_assistant;
    END IF;
END $$;
