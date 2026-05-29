-- Domain events table — records business facts as lightweight, append-only rows.
--
-- Consumers mark their own consumption via the `consumed_by` JSONB column:
--   e.g. {"assistant": "2026-05-28T10:00:00Z"}
-- Multiple consumers can each set their own key without affecting others.
--
-- Insertion is non-fatal: if the emit call fails the caller logs and continues.
-- The `domain_events` table is auxiliary; primary business state lives in the
-- canonical tables (inquiries, offers, invoices, …).

CREATE TABLE IF NOT EXISTS domain_events (
    id          uuid PRIMARY KEY,
    kind        text NOT NULL,
    aggregate   text NOT NULL,          -- e.g. 'inquiry:<uuid>', 'offer:<uuid>'
    payload     jsonb NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    consumed_by jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS idx_domain_events_kind    ON domain_events(kind);
CREATE INDEX IF NOT EXISTS idx_domain_events_created ON domain_events(created_at);
CREATE INDEX IF NOT EXISTS idx_domain_events_unconsumed_assistant
    ON domain_events(created_at)
    WHERE NOT (consumed_by ? 'assistant');
