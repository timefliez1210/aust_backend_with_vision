-- Internal work items (training, maintenance, etc.) that need employee
-- assignment and hours tracking like moving inquiries.
CREATE TABLE calendar_items (
    id             UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    title          VARCHAR(255) NOT NULL,
    description    TEXT,
    category       VARCHAR(50)  NOT NULL DEFAULT 'internal',
    location       TEXT,
    scheduled_date DATE,
    duration_hours NUMERIC(5,2) NOT NULL DEFAULT 0,
    status         VARCHAR(50)  NOT NULL DEFAULT 'scheduled',
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_calendar_items_date ON calendar_items(scheduled_date);

-- Junction table mirroring inquiry_employees for employee assignment + hours.
CREATE TABLE calendar_item_employees (
    id               UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    calendar_item_id UUID         NOT NULL REFERENCES calendar_items(id) ON DELETE CASCADE,
    employee_id      UUID         NOT NULL REFERENCES employees(id) ON DELETE CASCADE,
    planned_hours    NUMERIC(5,2) NOT NULL DEFAULT 0,
    actual_hours     NUMERIC(5,2),
    notes            TEXT,
    created_at       TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    UNIQUE(calendar_item_id, employee_id)
);

CREATE INDEX idx_calendar_item_employees_item ON calendar_item_employees(calendar_item_id);
CREATE INDEX idx_calendar_item_employees_emp  ON calendar_item_employees(employee_id);
