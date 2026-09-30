-- Barzahlung on the invoice itself.
--
-- Some customers pay Alex in cash on the day. The Rechnung then has to say so: instead
-- of the Sparkasse bank block it states "Diese Rechnung wurde am <Datum> in bar
-- beglichen", which makes the document its own Quittung. NULL → normal invoice
-- (pay by transfer).
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS cash_paid_on DATE;

COMMENT ON COLUMN invoices.cash_paid_on IS
    'Day the invoice was paid in cash (Barzahlung). Set → the PDF prints the cash receipt line instead of the bank details. NULL → transfer.';
