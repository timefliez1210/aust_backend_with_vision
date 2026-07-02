-- Lightweight appointments linked to an inquiry, on their own (possibly
-- non-consecutive) dates — e.g. a Besichtigung (on-site survey) days or weeks
-- before the actual move. The move itself stays a contiguous
-- [scheduled_date, end_date] range on `inquiries`; these are *separate* dated
-- entries that render connected to the inquiry on the calendar.
--
-- Deliberately NOT crew/hours tracked (no *_employees junction): a Besichtigung
-- is a scouting visit, not a payroll work-day. It carries at most one optional
-- assignee. `kind` is free text (default 'besichtigung') on purpose — label
-- CHECK constraints on this codebase have churned before (calendar categories
-- went free-text in 20260527); only `status` gets a CHECK.
CREATE TABLE inquiry_appointments (
    id             UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    inquiry_id     UUID         NOT NULL REFERENCES inquiries(id) ON DELETE CASCADE,
    kind           VARCHAR(50)  NOT NULL DEFAULT 'besichtigung',
    scheduled_date DATE         NOT NULL,
    start_time     TIME,
    end_time       TIME,
    assignee_id    UUID         REFERENCES employees(id) ON DELETE SET NULL,
    location       TEXT,
    notes          TEXT,
    status         VARCHAR(50)  NOT NULL DEFAULT 'scheduled',
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    CONSTRAINT inquiry_appointments_status_check
        CHECK (status IN ('scheduled', 'done', 'cancelled')),
    CONSTRAINT inquiry_appointments_end_after_start
        CHECK (start_time IS NULL OR end_time IS NULL OR end_time > start_time)
);

CREATE INDEX idx_inquiry_appointments_inquiry ON inquiry_appointments(inquiry_id);
CREATE INDEX idx_inquiry_appointments_date    ON inquiry_appointments(scheduled_date);

CREATE TRIGGER update_inquiry_appointments_updated_at
    BEFORE UPDATE ON inquiry_appointments FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
