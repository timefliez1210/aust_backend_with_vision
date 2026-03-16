-- Link calendar items to customers (optional — internal items have no customer)
ALTER TABLE calendar_items
    ADD COLUMN IF NOT EXISTS customer_id UUID REFERENCES customers(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_calendar_items_customer_id ON calendar_items(customer_id);
