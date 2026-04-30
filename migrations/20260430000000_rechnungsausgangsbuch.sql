-- Add payment_method, notes, and due_date to invoices for Rechnungsausgangsbuch
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS payment_method VARCHAR(50) DEFAULT NULL;
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS notes TEXT DEFAULT NULL;
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS due_date DATE DEFAULT NULL;

COMMENT ON COLUMN invoices.payment_method IS 'Zahlungsart (EC, BAR, Überweisung, etc.)';
COMMENT ON COLUMN invoices.notes IS 'Bemerkungen — free-text admin notes';
COMMENT ON COLUMN invoices.due_date IS 'Fälligkeitsdatum — may differ from sent_at + payment terms';
