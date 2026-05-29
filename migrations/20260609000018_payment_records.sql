-- Payment ledger: records individual payment transactions against invoices.
--
-- Supports partial payments — the invoice is automatically marked 'paid' when
-- the cumulative sum of payment_records.amount_cents reaches the invoice total.
-- See InvoiceServiceImpl::record_payment for the update logic.

CREATE TABLE payment_records (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    invoice_id      uuid        NOT NULL REFERENCES invoices(id) ON DELETE RESTRICT,
    amount_cents    bigint      NOT NULL,
    paid_at         date        NOT NULL,
    method          text        NOT NULL,
    reference       text,
    notes           text,
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_payment_records_invoice ON payment_records(invoice_id);
CREATE INDEX idx_payment_records_paid_at ON payment_records(paid_at);
