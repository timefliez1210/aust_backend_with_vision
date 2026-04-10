-- Unified inquiry/customer model: billing address, recipient, service_type,
-- customer_type, company_name, house_number, parking_ban.
--
-- All changes are additive — no columns dropped, no data mutated.
-- Existing rows get sensible defaults.

-- ═══════════════════════════════════════════════════════════════════════════
-- 1. customers: add B2B support + billing address
-- ═══════════════════════════════════════════════════════════════════════════

ALTER TABLE customers
    ADD COLUMN IF NOT EXISTS customer_type VARCHAR(10) NOT NULL DEFAULT 'private'
        CHECK (customer_type IN ('private', 'business')),
    ADD COLUMN IF NOT EXISTS company_name TEXT,
    ADD COLUMN IF NOT EXISTS billing_address_id UUID REFERENCES addresses(id);

CREATE INDEX IF NOT EXISTS idx_customers_billing_address
    ON customers(billing_address_id);

-- ═══════════════════════════════════════════════════════════════════════════
-- 2. addresses: add house_number + parking_ban
-- ═══════════════════════════════════════════════════════════════════════════

ALTER TABLE addresses
    ADD COLUMN IF NOT EXISTS house_number VARCHAR(20),
    ADD COLUMN IF NOT EXISTS parking_ban BOOLEAN NOT NULL DEFAULT false;

-- ═══════════════════════════════════════════════════════════════════════════
-- 3. inquiries: service_type, submission_mode, recipient, billing, custom_fields
-- ═══════════════════════════════════════════════════════════════════════════

ALTER TABLE inquiries
    ADD COLUMN IF NOT EXISTS service_type VARCHAR(50)
        CHECK (service_type IN (
            'privatumzug', 'firmenumzug', 'seniorenumzug', 'umzugshelfer',
            'montage', 'haushaltsaufloesung', 'entruempelung', 'lagerung'
        )),
    ADD COLUMN IF NOT EXISTS submission_mode VARCHAR(20) NOT NULL DEFAULT 'termin'
        CHECK (submission_mode IN ('termin', 'manuell', 'foto', 'video')),
    ADD COLUMN IF NOT EXISTS recipient_id UUID REFERENCES customers(id),
    ADD COLUMN IF NOT EXISTS billing_address_id UUID REFERENCES addresses(id),
    ADD COLUMN IF NOT EXISTS custom_fields JSONB NOT NULL DEFAULT '{}';

CREATE INDEX IF NOT EXISTS idx_inquiries_service_type
    ON inquiries(service_type);
CREATE INDEX IF NOT EXISTS idx_inquiries_recipient_id
    ON inquiries(recipient_id);
CREATE INDEX IF NOT EXISTS idx_inquiries_billing_address_id
    ON inquiries(billing_address_id);