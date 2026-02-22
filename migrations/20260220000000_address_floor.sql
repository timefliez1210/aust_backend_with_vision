-- Add floor information to addresses (needed for moving cost calculation)
ALTER TABLE addresses ADD COLUMN floor VARCHAR(50);
