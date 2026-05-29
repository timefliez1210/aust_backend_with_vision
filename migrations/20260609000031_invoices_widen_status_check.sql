-- B3: widen invoices.status CHECK to include the lifecycle states the
-- assistant's update_invoice_status tool already advertises in its enum:
-- 'overdue' (reminder cycle exhausted, customer past due), 'written_off'
-- (uncollectible, accounting close-out), 'void' (issued in error, cancelled).
--
-- The invoice.overdue domain event handler likewise has no path to fire
-- without an 'overdue' status reachable in the schema.
--
-- Additive: drop the old CHECK constraint by name, re-add a wider one. No
-- rows transition automatically — existing data already conforms to the
-- narrower set.

ALTER TABLE invoices DROP CONSTRAINT IF EXISTS invoices_status_check;

ALTER TABLE invoices ADD CONSTRAINT invoices_status_check
    CHECK (status IN ('draft', 'ready', 'sent', 'paid', 'overdue', 'written_off', 'void'));
