-- Leistungszeitraum carried on the invoice itself.
--
-- A generated invoice reaches its job dates through the inquiry. A ledger row imported
-- from Alex's Rechnungsausgangsbuch has no inquiry (see 20260823100000), yet his book
-- records a Leistungsdatum for every one of them — and the register books revenue into
-- the month of that date, not the invoice or payment date. Without a place to keep it,
-- 41 imported rows would show no Leistungszeitraum and land in the wrong month.
--
-- Readers resolve `COALESCE(invoices.service_start, inquiries.scheduled_date)`, so every
-- existing row keeps resolving exactly as before.
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS service_start DATE;
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS service_end DATE;

COMMENT ON COLUMN invoices.service_start IS
    'First day of the Leistungszeitraum when it is not derived from the inquiry (imported ledger rows). NULL → inquiries.scheduled_date.';
COMMENT ON COLUMN invoices.service_end IS
    'Last day of the Leistungszeitraum; NULL with service_start set means a single day.';
