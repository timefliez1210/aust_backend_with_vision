-- Flash contact table — ultra-quick customer callback requests
-- from the public landing page.

CREATE TABLE flash_contacts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    phone TEXT NOT NULL,
    time_preference TEXT NOT NULL CHECK (time_preference IN ('any_time', '08-10', '10-12', '14-16', '16-18')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    reminder_sent_at TIMESTAMPTZ,
    handled_at TIMESTAMPTZ
);

CREATE INDEX idx_flash_contacts_pending_reminders
ON flash_contacts(handled_at, reminder_sent_at, time_preference, created_at);
