-- Rechnungsausgangsbuch parity with Alex's Excel book.
--
-- Four additive changes, all driven by columns he actually maintains by hand in
-- "Rechnungsausgangsbuch 2024.xlsx" (sheet "2026") and that the app could not hold:
--
--   1. Teilzahlung   — 12 of his 86 rows carry a partial payment ("1300 Teilzahlung"
--                      on 2026-23). The app only knew paid / not paid.
--   2. Bemerkungen   — storage invoices had no notes column at all, so half the
--                      register could not take a remark.
--   3. Per-year numbering — his numbers restart at -01 every January. `invoice_number_seq`
--                      is a single global sequence that would have handed out
--                      "2027-0087" on 1 January.
--
-- No CHECK constraints are added to existing columns here: every new column is
-- nullable and every existing row is already valid (see the 2026-05-28 incident).

-- 1. Partial payments -------------------------------------------------------
--
-- Amount actually received so far, in cents. NULL means "no partial payment
-- recorded": the row is fully paid when `paid_at` is set and fully open otherwise.
-- That keeps every pre-existing row correct without having to recompute Brutto
-- here — Brutto is derived in Rust (`compute_invoice_amounts`), not stored.
ALTER TABLE invoices
    ADD COLUMN IF NOT EXISTS paid_amount_cents BIGINT;

ALTER TABLE storage_invoices
    ADD COLUMN IF NOT EXISTS paid_amount_cents BIGINT;

-- 2. Remarks on storage invoices --------------------------------------------
ALTER TABLE storage_invoices
    ADD COLUMN IF NOT EXISTS notes TEXT;

-- 3. Per-year invoice numbering ---------------------------------------------
--
-- Replaces the global `invoice_number_seq` as the allocator. The sequence itself is
-- deliberately left in place: `settings_repo` still reads it, older code paths may
-- reference it, and dropping it would be a destructive change to an applied schema.
CREATE TABLE IF NOT EXISTS invoice_number_counters (
    year       INT    PRIMARY KEY,
    last_value BIGINT NOT NULL
);

COMMENT ON TABLE invoice_number_counters IS
    'Per-calendar-year invoice counter. last_value = highest number handed out for that year; the next allocation is last_value + 1.';

-- Seed from the numbers already issued, so the first allocation after this
-- migration continues where the register left off rather than colliding.
-- Both register tables share the number space, hence the UNION.
INSERT INTO invoice_number_counters (year, last_value)
SELECT year, MAX(seq)
FROM (
    SELECT split_part(invoice_number, '-', 1)::INT    AS year,
           split_part(invoice_number, '-', 2)::BIGINT AS seq
    FROM invoices
    WHERE invoice_number ~ '^[0-9]{4}-[0-9]+$'
    UNION ALL
    SELECT split_part(invoice_number, '-', 1)::INT,
           split_part(invoice_number, '-', 2)::BIGINT
    FROM storage_invoices
    WHERE invoice_number ~ '^[0-9]{4}-[0-9]+$'
) issued
GROUP BY year
ON CONFLICT (year) DO NOTHING;

-- The global sequence can legitimately sit ahead of every issued number (a number
-- was drawn but the invoice insert then failed). Carry that high-water mark over
-- for the current year so a reserved-then-abandoned number is never reused.
-- `is_called = false` means the sequence has never been drawn from — on a fresh
-- database there is nothing to carry over and last_value is a placeholder.
INSERT INTO invoice_number_counters (year, last_value)
SELECT EXTRACT(YEAR FROM CURRENT_DATE)::INT, last_value
FROM invoice_number_seq
WHERE is_called
ON CONFLICT (year) DO UPDATE
    SET last_value = GREATEST(invoice_number_counters.last_value, EXCLUDED.last_value);

-- 4. Zahlungsart vocabulary --------------------------------------------------
--
-- Alex writes exactly "EC" (83x) and "BAR" (3x). The app offered "EC-Karte" and
-- "Bar", so not one stored value matched what he reads in his own book.
UPDATE invoices         SET payment_method = 'EC'  WHERE payment_method = 'EC-Karte';
UPDATE invoices         SET payment_method = 'BAR' WHERE payment_method = 'Bar';
UPDATE storage_invoices SET payment_method = 'EC'  WHERE payment_method = 'EC-Karte';
UPDATE storage_invoices SET payment_method = 'BAR' WHERE payment_method = 'Bar';
