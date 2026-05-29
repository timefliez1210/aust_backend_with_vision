-- Add `merged_into` FK column to customers.
--
-- When a customer is merged into another via the assistant `merge_customers` tool,
-- the merged-away customer gets `merged_into = <keep_id>`. Downstream reads (e.g.
-- customer auth login) check this column and reject login attempts for merged accounts.
ALTER TABLE customers
    ADD COLUMN IF NOT EXISTS merged_into UUID REFERENCES customers(id);

CREATE INDEX IF NOT EXISTS idx_customers_merged_into
    ON customers(merged_into)
    WHERE merged_into IS NOT NULL;
