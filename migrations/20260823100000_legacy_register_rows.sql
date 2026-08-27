-- Ledger-only invoice rows, for importing the years Alex invoiced outside the app.
--
-- His Excel Rechnungsausgangsbuch is the authoritative record for 2026: it holds 86
-- invoices, of which the app only ever generated 46. The other 40 were written and
-- sent by hand, so they have no inquiry, no offer and no PDF — they are records, not
-- jobs, and they will never gain one.
--
-- `invoices.inquiry_id` was NOT NULL, so there was nowhere to put them. The
-- alternative — inventing 40 placeholder inquiries — would have seeded fake jobs into
-- the calendar, the pipeline and every customer's history permanently. Making the FK
-- optional and carrying the customer directly is the honest shape.

-- 1. An invoice no longer requires an inquiry ------------------------------
--
-- Every existing row keeps its inquiry; only imported rows leave it NULL. The
-- register query has LEFT JOINed inquiries since feedback report a61982f1, so it
-- already tolerates their absence.
ALTER TABLE invoices ALTER COLUMN inquiry_id DROP NOT NULL;

-- 2. Customer, carried directly ---------------------------------------------
--
-- Normally the customer is reached through the inquiry. A ledger row has no inquiry,
-- so it names its customer itself. Readers resolve
-- `COALESCE(invoices.customer_id, inquiries.customer_id)`, which leaves every
-- existing row resolving exactly as before.
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS customer_id UUID REFERENCES customers(id);

CREATE INDEX IF NOT EXISTS idx_invoices_customer_id
    ON invoices(customer_id) WHERE customer_id IS NOT NULL;

-- 3. Imported-record marker --------------------------------------------------
--
-- TRUE for a row that came from the spreadsheet rather than from this system. Such a
-- row has no PDF to open and no pipeline to advance, and must never be picked up by
-- the dunning tick — chasing payment on an invoice the app never sent would email a
-- customer about something it cannot show them.
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS is_legacy BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN invoices.is_legacy IS
    'TRUE for rows imported from the historical Rechnungsausgangsbuch: no inquiry, no offer, no PDF, excluded from dunning.';
