-- Runtime-editable settings (key/value JSONB).
--
-- Currently used for standard pricing values so they can be changed without a
-- redeploy. Invoice/KVA numbers are NOT stored here — they remain Postgres
-- sequences (invoice_number_seq, offer_number_seq); the settings UI sets the
-- "next number" via setval.
--
-- Pricing keys mirror CompanyConfig fields; when a key is absent the backend
-- falls back to the config/env default.
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
