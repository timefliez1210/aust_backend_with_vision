-- Free-form employee documents: Alex types a label and uploads a file.
--
-- The two fixed columns on `employees` (arbeitsvertrag_key, mitarbeiterfragebogen_key)
-- stay exactly as they are. This table sits beside them and holds everything else a
-- personnel file collects — Führungszeugnis, Fahrerlaubnis, Krankmeldung, whatever
-- the office needs next — without a migration per document type.
CREATE TABLE IF NOT EXISTS employee_documents (
    id           UUID PRIMARY KEY,
    employee_id  UUID NOT NULL REFERENCES employees(id) ON DELETE CASCADE,
    -- The label Alex typed. It is what the card shows, so it is required.
    label        TEXT NOT NULL,
    -- S3 object key. Unique because each upload writes its own key.
    s3_key       TEXT NOT NULL,
    -- Original filename as it left Alex's machine, for the download's Content-Disposition.
    filename     TEXT NOT NULL,
    content_type TEXT NOT NULL DEFAULT 'application/octet-stream',
    size_bytes   BIGINT NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT employee_documents_label_not_blank CHECK (LENGTH(TRIM(label)) > 0)
);

CREATE INDEX IF NOT EXISTS idx_employee_documents_employee
    ON employee_documents(employee_id, created_at DESC);
