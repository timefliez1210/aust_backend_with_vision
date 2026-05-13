-- Snooze / dismiss support for flash contact Telegram bot.
-- next_remind_at: set by the bot when Alex taps "Nochmal erinnern".
-- dismissed_at:   set by the bot when Alex taps "Verwerfen".

ALTER TABLE flash_contacts
    ADD COLUMN next_remind_at  TIMESTAMPTZ,
    ADD COLUMN dismissed_at    TIMESTAMPTZ;

-- Fast lookup for the cron: contacts that need a reminder soon.
CREATE INDEX idx_flash_contacts_next_remind
    ON flash_contacts (next_remind_at)
    WHERE handled_at IS NULL AND dismissed_at IS NULL;
