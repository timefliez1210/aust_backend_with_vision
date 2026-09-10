-- Correct the column comment on invoices.line_items_json.
--
-- The comment written in 20260414000000 documents the shape
--   {pos, description, quantity, unit_price, remark}
-- but the code has always written and read
--   {description, quantity, unit_price_cents, remark}
-- (`ManualLineItem` in crates/api/src/routes/invoices.rs). Both readers parse with
-- `unwrap_or_default()`, so a row written in the documented shape does not fail — it
-- yields an invoice with no positions and a 0,00 € total, which is worse.
--
-- Only the comment changes. The earlier migration is left untouched: sqlx checksums
-- the whole file body, so editing an applied migration is a VersionMismatch on deploy.

COMMENT ON COLUMN invoices.line_items_json IS
'Hand-edited invoice positions as a JSON array, present only when is_manual = TRUE. Each item: {description, quantity, unit_price_cents, remark}. unit_price_cents is netto in cents; quantity is a real number so worked hours like 12.5 render as "12,5". NULL for offer-derived invoices.';
