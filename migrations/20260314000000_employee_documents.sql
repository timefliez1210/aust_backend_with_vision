-- Add document storage keys to employees.
-- Null means the document has not been uploaded yet.
-- Values are S3 object keys.
ALTER TABLE employees
    ADD COLUMN arbeitsvertrag_key        TEXT,
    ADD COLUMN mitarbeiterfragebogen_key TEXT;
