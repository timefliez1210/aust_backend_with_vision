-- Calendar module: bookings and capacity overrides
-- Tracks confirmed/tentative moving jobs and per-day capacity adjustments.

CREATE TABLE calendar_bookings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    booking_date DATE NOT NULL,
    quote_id UUID REFERENCES quotes(id) ON DELETE SET NULL,
    customer_name VARCHAR(255),
    customer_email VARCHAR(255),
    departure_address TEXT,
    arrival_address TEXT,
    volume_m3 DOUBLE PRECISION,
    distance_km DOUBLE PRECISION,
    description TEXT,
    status VARCHAR(50) NOT NULL DEFAULT 'confirmed'
        CHECK (status IN ('tentative', 'confirmed', 'cancelled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_calendar_bookings_date ON calendar_bookings(booking_date);
CREATE INDEX idx_calendar_bookings_status ON calendar_bookings(status);
CREATE INDEX idx_calendar_bookings_quote_id ON calendar_bookings(quote_id);

CREATE TRIGGER update_calendar_bookings_updated_at
    BEFORE UPDATE ON calendar_bookings
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

-- Days where capacity differs from the default (e.g. Alex has 2 trucks)
CREATE TABLE calendar_capacity_overrides (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    override_date DATE NOT NULL UNIQUE,
    capacity INT NOT NULL CHECK (capacity >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_calendar_capacity_overrides_date ON calendar_capacity_overrides(override_date);
