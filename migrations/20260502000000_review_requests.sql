-- Tracks whether Alex has sent (or scheduled) a Google-review request email
-- after an inquiry is marked completed.
--
-- Status values:
--   sent     — email was dispatched immediately
--   pending  — "Später" chosen; remind_after is the date to surface the reminder
--   skipped  — Alex chose not to send

CREATE TABLE review_requests (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    inquiry_id  UUID        NOT NULL UNIQUE REFERENCES inquiries(id) ON DELETE CASCADE,
    status      TEXT        NOT NULL DEFAULT 'pending'
                            CHECK (status IN ('pending', 'sent', 'skipped')),
    remind_after DATE,
    sent_at     TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_review_requests_remind_after
    ON review_requests (remind_after)
    WHERE status = 'pending' AND remind_after IS NOT NULL;
