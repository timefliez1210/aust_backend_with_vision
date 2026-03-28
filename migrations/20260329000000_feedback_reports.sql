-- Feedback reports: bugs and feature requests submitted via the admin dashboard.
CREATE TABLE feedback_reports (
    id               UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    report_type      VARCHAR(20) NOT NULL CHECK (report_type IN ('bug', 'feature')),
    priority         VARCHAR(20) NOT NULL DEFAULT 'medium'
                                 CHECK (priority IN ('low', 'medium', 'high', 'critical')),
    title            TEXT        NOT NULL,
    description      TEXT,
    location         TEXT,
    attachment_keys  TEXT[]      NOT NULL DEFAULT '{}',
    status           VARCHAR(20) NOT NULL DEFAULT 'open'
                                 CHECK (status IN ('open', 'in_progress', 'resolved')),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_feedback_reports_status     ON feedback_reports(status);
CREATE INDEX idx_feedback_reports_created_at ON feedback_reports(created_at DESC);
