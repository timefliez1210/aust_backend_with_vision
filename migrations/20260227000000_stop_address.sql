-- Add intermediate stop address to quotes
ALTER TABLE quotes ADD COLUMN stop_address_id UUID REFERENCES addresses(id);
