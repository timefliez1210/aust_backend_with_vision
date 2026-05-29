-- Add a free-form notes field to customers for the assistant `add_customer_note` tool.
-- Additive: existing rows get NULL.
ALTER TABLE customers
    ADD COLUMN IF NOT EXISTS notes TEXT;
