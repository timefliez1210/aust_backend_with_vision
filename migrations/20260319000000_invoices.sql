-- Invoice number sequence (separate from offer_number_seq)
-- Format: {seq}{year}, e.g. "12026", "22026"
CREATE SEQUENCE invoice_number_seq START WITH 1;

-- Invoices: one per inquiry (full) or two per inquiry (partial pair)
--
-- Amounts are NOT stored here — they are computed at generation time from:
--   offers.price_cents (netto) + partial_percent + extra_services
--
-- invoice_type:
--   full           — single invoice for the full amount + any extras
--   partial_first  — Anzahlung (downpayment), sendable immediately
--   partial_final  — Restbetrag, sendable only after inquiry.status = 'completed'
--
-- extra_services JSONB shape:
--   [{"description": "Klaviertransport", "price_cents": 15000}, ...]
--   price_cents = netto amount; template adds 19% MwSt on top
--   Only populated on invoice_type = 'full' or 'partial_final'
CREATE TABLE invoices (
    id               UUID PRIMARY KEY,
    inquiry_id       UUID NOT NULL REFERENCES inquiries(id) ON DELETE CASCADE,
    invoice_number   TEXT NOT NULL UNIQUE,
    invoice_type     VARCHAR(20) NOT NULL
                       CHECK (invoice_type IN ('full', 'partial_first', 'partial_final')),
    partial_group_id UUID,                  -- links the two invoices in a partial pair
    partial_percent  INTEGER,               -- e.g. 30 — only set on partial_first
    status           VARCHAR(20) NOT NULL DEFAULT 'draft'
                       CHECK (status IN ('draft', 'ready', 'sent', 'paid')),
    extra_services   JSONB NOT NULL DEFAULT '[]',
    pdf_s3_key       TEXT,
    sent_at          TIMESTAMPTZ,
    paid_at          TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_invoices_inquiry_id    ON invoices(inquiry_id);
CREATE INDEX idx_invoices_partial_group ON invoices(partial_group_id);
