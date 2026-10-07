-- A company's letterhead: what its offers, invoices and forms print about it.
--
-- Additive only. Another company's templates are derived from Aust's with these
-- details and its logo swapped in (crates/offer-generator/src/letterhead.rs).
-- Aust keeps its compiled-in templates; its values are filled in here for the
-- record (they are what those templates print).

ALTER TABLE tenants
    ADD COLUMN street      TEXT NOT NULL DEFAULT '',
    ADD COLUMN postal_code TEXT NOT NULL DEFAULT '',
    ADD COLUMN email       TEXT NOT NULL DEFAULT '',
    ADD COLUMN website     TEXT NOT NULL DEFAULT '',
    ADD COLUMN agb_url     TEXT NOT NULL DEFAULT '',
    ADD COLUMN bank_name   TEXT NOT NULL DEFAULT '',
    ADD COLUMN iban        TEXT NOT NULL DEFAULT '',
    ADD COLUMN bic         TEXT NOT NULL DEFAULT '',
    ADD COLUMN tax_number  TEXT NOT NULL DEFAULT '',
    ADD COLUMN vat_id      TEXT NOT NULL DEFAULT '',
    ADD COLUMN logo        BYTEA;

UPDATE tenants SET
    street      = 'Ehrlicherstr. 38',
    postal_code = '31135',
    email       = 'info@aust-umzuege.de',
    website     = 'www.aust-umzuege.de',
    agb_url     = 'www.aust-umzuege.de/rechtliches/agbs',
    bank_name   = 'Sparkasse Hildesheim Goslar Peine',
    iban        = 'DE67 2595 0130 0057 4537 49',
    bic         = 'NOLADE21HIK',
    tax_number  = '30/101/22146',
    vat_id      = 'DE330779170'
WHERE id = '0190aa57-0000-7000-8000-000000000001';
