-- Per-day start/end times for inquiry_days and calendar_item_days.
-- NULL means "inherit from parent inquiry/calendar_item".
-- Per-day employee assignment tables so different staff can be
-- assigned on each day of a multi-day appointment.

ALTER TABLE inquiry_days ADD COLUMN start_time TIME;
ALTER TABLE inquiry_days ADD COLUMN end_time   TIME;

ALTER TABLE calendar_item_days ADD COLUMN start_time TIME;
ALTER TABLE calendar_item_days ADD COLUMN end_time   TIME;

-- Per-day employee assignments for inquiry days.
-- Cascade-deletes when the parent inquiry_day is deleted.
CREATE TABLE inquiry_day_employees (
    id             UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    inquiry_day_id UUID        NOT NULL REFERENCES inquiry_days(id) ON DELETE CASCADE,
    employee_id    UUID        NOT NULL REFERENCES employees(id)    ON DELETE CASCADE,
    planned_hours  NUMERIC(5,2),
    notes          TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(inquiry_day_id, employee_id)
);
CREATE INDEX idx_inquiry_day_employees_day ON inquiry_day_employees(inquiry_day_id);

-- Per-day employee assignments for calendar item days.
-- Cascade-deletes when the parent calendar_item_day is deleted.
CREATE TABLE calendar_item_day_employees (
    id                   UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    calendar_item_day_id UUID        NOT NULL REFERENCES calendar_item_days(id) ON DELETE CASCADE,
    employee_id          UUID        NOT NULL REFERENCES employees(id)           ON DELETE CASCADE,
    planned_hours        NUMERIC(5,2),
    notes                TEXT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(calendar_item_day_id, employee_id)
);
CREATE INDEX idx_calendar_item_day_employees_day ON calendar_item_day_employees(calendar_item_day_id);
