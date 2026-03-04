-- Lifecycle refactor: Unified Inquiry Entity
-- Renames quotes -> inquiries, adds services JSONB, lifecycle timestamps

-- 1. Add new columns
ALTER TABLE quotes
    ADD COLUMN IF NOT EXISTS services JSONB NOT NULL DEFAULT '{}',
    ADD COLUMN IF NOT EXISTS source VARCHAR(50) NOT NULL DEFAULT 'direct_email',
    ADD COLUMN IF NOT EXISTS offer_sent_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS accepted_at TIMESTAMPTZ;

-- 2. Backfill services from notes
UPDATE quotes SET services = jsonb_build_object(
    'packing', COALESCE(notes ILIKE '%verpackungsservice%' OR notes ILIKE '%einpackservice%', false),
    'assembly', COALESCE(notes ~ '(?<![Dd]e)[Mm]ontage', false),
    'disassembly', COALESCE(notes ILIKE '%demontage%', false),
    'storage', COALESCE(notes ILIKE '%einlagerung%', false),
    'disposal', COALESCE(notes ILIKE '%entsorgung%', false),
    'parking_ban_origin', COALESCE(notes ILIKE '%halteverbot auszug%' OR notes ILIKE '%halteverbot beladestelle%', false),
    'parking_ban_destination', COALESCE(notes ILIKE '%halteverbot einzug%' OR notes ILIKE '%halteverbot entladestelle%', false)
) WHERE notes IS NOT NULL;

-- 3. Backfill lifecycle timestamps from offers table
UPDATE quotes SET offer_sent_at = (
    SELECT o.sent_at FROM offers o WHERE o.quote_id = quotes.id
    AND o.sent_at IS NOT NULL ORDER BY o.created_at DESC LIMIT 1
) WHERE status IN ('offer_sent','accepted','rejected','expired','done','paid');

UPDATE quotes SET accepted_at = updated_at
WHERE status IN ('accepted','done','paid');

-- 4. Map old status values to new names
UPDATE quotes SET status = 'estimated' WHERE status = 'volume_estimated';
UPDATE quotes SET status = 'offer_ready' WHERE status = 'offer_generated';

-- 5. Rename table
ALTER TABLE quotes RENAME TO inquiries;

-- 6. Rename FK columns in related tables
ALTER TABLE volume_estimations RENAME COLUMN quote_id TO inquiry_id;
ALTER TABLE offers RENAME COLUMN quote_id TO inquiry_id;
ALTER TABLE email_threads RENAME COLUMN quote_id TO inquiry_id;
ALTER TABLE calendar_bookings RENAME COLUMN quote_id TO inquiry_id;

-- 7. Rename indexes
ALTER INDEX IF EXISTS idx_quotes_customer_id RENAME TO idx_inquiries_customer_id;
ALTER INDEX IF EXISTS idx_quotes_status RENAME TO idx_inquiries_status;
ALTER INDEX IF EXISTS idx_quotes_created_at RENAME TO idx_inquiries_created_at;

-- 8. Recreate unique constraints with new names
DROP INDEX IF EXISTS offers_quote_active_unique;
CREATE UNIQUE INDEX offers_inquiry_active_unique
    ON offers(inquiry_id) WHERE status NOT IN ('rejected','cancelled');

DROP INDEX IF EXISTS idx_calendar_bookings_quote_active;
CREATE UNIQUE INDEX idx_calendar_bookings_inquiry_active
    ON calendar_bookings(inquiry_id) WHERE inquiry_id IS NOT NULL AND status != 'cancelled';
