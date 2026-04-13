-- Add line_items_json column to invoices table.
-- Migrates from the simple extra_services model (lump-sum base + extras)
-- to a full line-items model where each service is an individual line item,
-- supporting positive Zusatzleistungen and negative Gutschriften/Rückerstattungen.
--
-- Backward compatibility: existing extra_services data is preserved.
-- The API will prefer line_items_json when present, falling back to
-- converting extra_services + base amount to line items for old invoices.

ALTER TABLE invoices ADD COLUMN IF NOT EXISTS line_items_json JSONB DEFAULT NULL;

-- Migrate existing invoices: convert their base amount + extra_services
-- into explicit line items. For 'full' invoices, the base is the offer price;
-- for 'partial_first', a single Anzahlung line; for 'partial_final', the
-- remainder minus a deduction line. We only have the extra_services JSON here,
-- so we store them as line items and leave the base to the application layer.

-- We'll perform the migration in the application layer on first read,
-- rather than trying to do the math in SQL (which would need offer data).
-- The column starts as NULL for all existing invoices; the API will
-- populate it lazily or on the next update.

COMMENT ON COLUMN invoices.line_items_json IS
'Itemised invoice line items as JSON array. Each item: {pos, description, quantity, unit_price, remark}. NULL for pre-migration invoices — converted on first access.';