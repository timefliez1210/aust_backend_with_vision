-- Calendar refactor: drop calendar_bookings, add scheduled_date to inquiries.
--
-- calendar_bookings was a denormalized cache of inquiry data that created drift
-- and was only auto-created when preferred_date was set via the email pipeline.
-- Inquiries are now the single source of truth for calendar availability.
-- capacity_overrides stays — it is the only calendar-specific data.

-- 1. Add scheduled_date: the date Alex actually locks in (may differ from preferred_date)
ALTER TABLE inquiries
    ADD COLUMN IF NOT EXISTS scheduled_date DATE;

CREATE INDEX IF NOT EXISTS idx_inquiries_scheduled_date ON inquiries(scheduled_date);

-- 2. Backfill scheduled_date from any existing calendar_bookings that had one
UPDATE inquiries i
SET scheduled_date = cb.booking_date
FROM calendar_bookings cb
WHERE cb.inquiry_id = i.id
  AND cb.status != 'cancelled'
  AND i.scheduled_date IS NULL;

-- 3. Drop calendar_bookings and its indexes/triggers (data now lives in inquiries)
DROP TABLE IF EXISTS calendar_bookings CASCADE;
