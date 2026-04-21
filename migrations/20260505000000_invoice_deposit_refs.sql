-- Add deposit reference columns to the invoices table.
--
-- deposit_percent: the Anzahlung percentage stored on the partial_first row so
--   the final invoice can reference it without a sibling lookup.
-- deposit_invoice_id: FK on the partial_final row pointing to its sibling
--   partial_first invoice.  Used to render "Abzüglich Anzahlung gemäß
--   Rechnung Nr. {number}" on the Schlussrechnung.
--
-- Both columns are nullable so existing rows are unaffected (additive-only).

ALTER TABLE invoices
    ADD COLUMN IF NOT EXISTS deposit_percent    SMALLINT  DEFAULT NULL,
    ADD COLUMN IF NOT EXISTS deposit_invoice_id UUID      DEFAULT NULL;

COMMENT ON COLUMN invoices.deposit_percent IS
'Anzahlung percentage (e.g. 30) stored on the partial_first row. NULL for full invoices.';

COMMENT ON COLUMN invoices.deposit_invoice_id IS
'FK to the partial_first invoice for the matching partial_final row. NULL for all other invoice types.';
