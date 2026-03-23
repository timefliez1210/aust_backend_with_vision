-- Add composite and covering indexes for common query patterns
--
-- Notes on existing indexes:
--   idx_inquiries_status        — single-column on status
--   idx_inquiries_created_at    — single-column on created_at DESC
--   idx_offers_quote_id         — single-column on offers(inquiry_id) [legacy name after column rename]
--   idx_volume_estimations_quote_id — single-column on volume_estimations(inquiry_id) [legacy name]
--   idx_email_threads_quote_id  — single-column on email_threads(inquiry_id) [legacy name]
--   idx_inquiry_employees_inquiry — single-column on inquiry_employees(inquiry_id) [already exists]
--
-- New indexes replace/complement the above for multi-column query patterns:

-- Composite index for list queries filtered by status, ordered by created_at DESC.
-- Replaces the need to merge idx_inquiries_status + idx_inquiries_created_at.
CREATE INDEX IF NOT EXISTS idx_inquiries_status_created
    ON inquiries(status, created_at DESC);

-- Composite index for offer lookups by inquiry, ordered newest first.
-- Complements the existing single-column idx_offers_quote_id.
CREATE INDEX IF NOT EXISTS idx_offers_inquiry_created
    ON offers(inquiry_id, created_at DESC);

-- Single-column index on volume_estimations(inquiry_id) with the canonical name.
-- idx_volume_estimations_quote_id already covers this column (legacy name after
-- column rename); this ensures the index exists even if that old index is dropped.
CREATE INDEX IF NOT EXISTS idx_volume_estimations_inquiry
    ON volume_estimations(inquiry_id);

-- Single-column index on email_threads(inquiry_id) with the canonical name.
-- idx_email_threads_quote_id already covers this column (legacy name after rename).
CREATE INDEX IF NOT EXISTS idx_email_threads_inquiry
    ON email_threads(inquiry_id);

-- idx_inquiry_employees_inquiry already exists from 20260306000000_employees.sql,
-- but we use IF NOT EXISTS for safety.
CREATE INDEX IF NOT EXISTS idx_inquiry_employees_inquiry
    ON inquiry_employees(inquiry_id);
