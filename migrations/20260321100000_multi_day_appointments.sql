-- Multi-day appointment support for inquiries and calendar items.
-- When inquiry_days is empty, the inquiry is treated as single-day using
-- inquiries.scheduled_date + start_time/end_time (backward compatible).
-- When populated, the schedule endpoint expands the inquiry across all listed dates.

CREATE TABLE inquiry_days (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    inquiry_id   UUID        NOT NULL REFERENCES inquiries(id) ON DELETE CASCADE,
    day_date     DATE        NOT NULL,
    day_number   SMALLINT    NOT NULL DEFAULT 1 CHECK (day_number >= 1),
    notes        TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(inquiry_id, day_date),
    UNIQUE(inquiry_id, day_number)
);

CREATE INDEX idx_inquiry_days_inquiry ON inquiry_days(inquiry_id);
CREATE INDEX idx_inquiry_days_date    ON inquiry_days(day_date);

-- Same structure for calendar items (Termine).
CREATE TABLE calendar_item_days (
    id               UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    calendar_item_id UUID        NOT NULL REFERENCES calendar_items(id) ON DELETE CASCADE,
    day_date         DATE        NOT NULL,
    day_number       SMALLINT    NOT NULL DEFAULT 1 CHECK (day_number >= 1),
    notes            TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(calendar_item_id, day_date),
    UNIQUE(calendar_item_id, day_number)
);

CREATE INDEX idx_calendar_item_days_item ON calendar_item_days(calendar_item_id);
CREATE INDEX idx_calendar_item_days_date ON calendar_item_days(day_date);
