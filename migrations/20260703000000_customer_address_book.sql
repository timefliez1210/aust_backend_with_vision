-- Customer address book: a reusable per-customer catalogue of known addresses.
--
-- Motivation: customers can exist without any inquiry, and the admin wants to
-- attach known addresses (from past jobs, correspondence, manual entry) to a
-- customer and pick one when creating a new inquiry from the inquiry overview
-- or the calendar.
--
-- These rows are SELF-CONTAINED copies, deliberately NOT a foreign key into
-- `addresses`. The `addresses` table holds per-inquiry snapshots that get
-- mutated in place (floor/elevator/parking edits via PATCH /admin/addresses).
-- If the book pointed at those rows, editing one inquiry would silently rewrite
-- the book. Creating an inquiry from a book entry copies the values into a
-- fresh `addresses` snapshot, preserving the existing snapshot semantics.
--
-- Additive only — no columns dropped, no data mutated.

CREATE TABLE IF NOT EXISTS customer_addresses (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id   UUID NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    street        VARCHAR(255) NOT NULL,
    house_number  VARCHAR(20),
    postal_code   VARCHAR(20),
    city          VARCHAR(100) NOT NULL,
    country       VARCHAR(100) NOT NULL DEFAULT 'Deutschland',
    floor         VARCHAR(50),
    elevator      BOOLEAN,
    parking_ban   BOOLEAN NOT NULL DEFAULT false,
    latitude      DOUBLE PRECISION,
    longitude     DOUBLE PRECISION,
    -- Optional human label ("Alte Wohnung", "Firma") shown in the picker.
    label         TEXT,
    -- Provenance: 'inquiry' (harvested), 'manual', or 'email' (future v2).
    source        VARCHAR(20) NOT NULL DEFAULT 'manual'
        CHECK (source IN ('inquiry', 'manual', 'email')),
    -- Most-recently-used sorts to the top of the picker.
    last_used_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_customer_addresses_customer
    ON customer_addresses(customer_id);

-- Dedup key: one entry per (customer, physical address). Case-insensitive on
-- street/city; house_number and postal_code coalesced so NULLs collapse.
-- lower() and coalesce() are IMMUTABLE, so they're valid in a unique index.
CREATE UNIQUE INDEX IF NOT EXISTS uniq_customer_address
    ON customer_addresses (
        customer_id,
        lower(street),
        coalesce(house_number, ''),
        coalesce(postal_code, ''),
        lower(city)
    );
