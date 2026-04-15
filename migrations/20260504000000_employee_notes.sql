-- Add admin-written notes that are visible to assigned employees in the worker portal.
-- Distinct from the existing `notes` field (customer-submitted service requests).

ALTER TABLE inquiries
    ADD COLUMN IF NOT EXISTS employee_notes TEXT;

ALTER TABLE calendar_items
    ADD COLUMN IF NOT EXISTS employee_notes TEXT;
