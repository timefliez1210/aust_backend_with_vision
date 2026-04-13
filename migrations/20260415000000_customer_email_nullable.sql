-- Make customer email optional so customers without email addresses can be created.
-- Older customers often don't have email; the field should not block customer creation.
-- PostgreSQL UNIQUE constraints allow multiple NULLs, so no index changes needed.

ALTER TABLE customers
    ALTER COLUMN email DROP NOT NULL;