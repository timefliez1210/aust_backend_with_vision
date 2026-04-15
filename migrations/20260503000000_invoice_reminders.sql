-- Tracks dunning reminders for unpaid invoices.
--
-- Created automatically when an invoice is sent (level 1, remind_after = sent_at + 7 days).
-- Admin acts on due reminders from the dashboard:
--   send  → email sent, level increments (max 3), next remind_after = today + 7 days
--   later → remind_after postponed by N days
--   paid  → invoice marked paid externally, reminder closed
--
-- Levels:
--   1 = Zahlungserinnerung  (7 days after sending)
--   2 = 1. Mahnung          (7 days after level-1 action)
--   3 = 2. Mahnung          (7 days after level-2 action)

CREATE TABLE invoice_reminders (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    invoice_id   UUID        NOT NULL UNIQUE REFERENCES invoices(id) ON DELETE CASCADE,
    level        INT         NOT NULL DEFAULT 1 CHECK (level BETWEEN 1 AND 3),
    status       TEXT        NOT NULL DEFAULT 'pending'
                             CHECK (status IN ('pending', 'sent', 'snoozed', 'closed')),
    remind_after DATE        NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_invoice_reminders_due
    ON invoice_reminders (remind_after)
    WHERE status = 'pending';
