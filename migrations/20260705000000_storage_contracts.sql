-- Storage-rental ("Lagerung") side quest: customers who rent storage space on a
-- monthly contract. Deliberately isolated from the core inquiry→offer→invoice
-- workflow — storage invoices live in their own table and never touch `invoices`.
-- The ONLY shared resource is the invoice-number space: storage invoices draw
-- from the same `invoice_number_seq` (created in 20260319000000_invoices.sql) so
-- the Rechnungsausgangsbuch stays gap-free and legally sequential.
--
-- Money is stored NETTO in cents (the XLSX template re-adds 19% MwSt); the UI
-- enters BRUTTO and converts on the way in, matching how Alex thinks everywhere.

CREATE TABLE storage_contracts (
    id                  UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id         UUID         NOT NULL REFERENCES customers(id) ON DELETE RESTRICT,
    -- Optional billing-address override; falls back to customers.billing_address_id.
    billing_address_id  UUID         REFERENCES addresses(id) ON DELETE SET NULL,
    contract_start      DATE         NOT NULL,
    contract_end        DATE,                       -- NULL = open-ended
    sqm                 NUMERIC(6,1) NOT NULL,      -- rented square metres (printed on the line item)
    monthly_netto_cents BIGINT       NOT NULL,      -- stored netto; entered brutto in the UI
    -- Day-of-month to bill on (anniversary billing). Clamped to 28 so it is valid
    -- in every month; derived from contract_start on insert.
    billing_day         SMALLINT     NOT NULL,
    status              VARCHAR(20)  NOT NULL DEFAULT 'active',
    note                TEXT,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    CONSTRAINT storage_contracts_status_check
        CHECK (status IN ('active', 'ended', 'cancelled')),
    CONSTRAINT storage_contracts_billing_day_check
        CHECK (billing_day BETWEEN 1 AND 28),
    CONSTRAINT storage_contracts_end_after_start
        CHECK (contract_end IS NULL OR contract_end >= contract_start)
);

CREATE INDEX idx_storage_contracts_customer ON storage_contracts(customer_id);
CREATE INDEX idx_storage_contracts_active   ON storage_contracts(status) WHERE status = 'active';

CREATE TRIGGER update_storage_contracts_updated_at
    BEFORE UPDATE ON storage_contracts FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

CREATE TABLE storage_invoices (
    id             UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    contract_id    UUID         NOT NULL REFERENCES storage_contracts(id) ON DELETE RESTRICT,
    invoice_number TEXT         NOT NULL UNIQUE,    -- shared invoice_number_seq: "YYYY-NNNN"
    period_year    INT          NOT NULL,
    period_month   INT          NOT NULL,           -- billed month (1-12)
    netto_cents    BIGINT       NOT NULL,
    pdf_s3_key     TEXT,
    status         VARCHAR(20)  NOT NULL DEFAULT 'pending_approval',
    payment_method TEXT,                            -- parity with invoices for the register
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    approved_at    TIMESTAMPTZ,
    sent_at        TIMESTAMPTZ,
    CONSTRAINT storage_invoices_status_check
        CHECK (status IN ('pending_approval', 'sent', 'paid', 'cancelled')),
    CONSTRAINT storage_invoices_month_check
        CHECK (period_month BETWEEN 1 AND 12),
    -- Idempotency guard: at most one invoice per contract per calendar month.
    -- The billing tick relies on this via INSERT ... ON CONFLICT DO NOTHING.
    CONSTRAINT storage_invoices_period_unique
        UNIQUE (contract_id, period_year, period_month)
);

CREATE INDEX idx_storage_invoices_contract ON storage_invoices(contract_id);
CREATE INDEX idx_storage_invoices_status   ON storage_invoices(status);
