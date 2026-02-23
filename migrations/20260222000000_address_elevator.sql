-- Add elevator field to addresses (affects pricing: floors without elevator add extra helpers)
ALTER TABLE addresses ADD COLUMN elevator BOOLEAN;
