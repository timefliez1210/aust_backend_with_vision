-- Manual invoice mode: let Alex fully edit an invoice's line items by hand
-- (feedback report 7c558577 — business customers want worked hours itemised as
-- "12,5 Stunden à 45,00 €", and the offer-derived invoice is not flexible enough).
--
-- When `is_manual` is TRUE, the invoice's line items come from the already-present
-- `line_items_json` column (added in 20260414000000, previously unused) instead of
-- being recomputed from the linked offer on every render. This flag is the guard
-- that stops the self-heal / regenerate paths from clobbering manual edits.
--
-- Scope v1: only `full` invoices are ever marked manual; partial invoices keep
-- their offer-derived split logic.
ALTER TABLE invoices
    ADD COLUMN IF NOT EXISTS is_manual BOOLEAN NOT NULL DEFAULT FALSE;
