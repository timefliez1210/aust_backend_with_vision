-- Fix inquiry_days and calendar_item_days rows that were saved 1 day early.
--
-- Root cause: applyInquiryDateRange / applyTerminDateRange used
-- cur.toISOString().slice(0,10) to build day_date strings. In UTC+8,
-- midnight local time is the previous day in UTC, so every day_date was
-- stored 1 day behind the intended value.
--
-- Detection: if the earliest day_date for a parent equals scheduled_date - 1 day,
-- all its day rows are off by exactly -1. Shift them +1.
-- Correctly-set rows (min day_date = scheduled_date) are left untouched.
--
-- Processing order: descending day_date per parent avoids unique constraint
-- violations — the latest row vacates its slot before the next one moves in.

DO $$
DECLARE
    r RECORD;
BEGIN
    -- Fix inquiry_days
    FOR r IN
        SELECT id2.id, id2.day_date
        FROM inquiry_days id2
        JOIN inquiries i ON i.id = id2.inquiry_id
        WHERE i.scheduled_date IS NOT NULL
          AND (
              SELECT MIN(day_date) FROM inquiry_days
              WHERE inquiry_id = i.id
          ) = i.scheduled_date - INTERVAL '1 day'
        ORDER BY id2.day_date DESC
    LOOP
        UPDATE inquiry_days SET day_date = r.day_date + INTERVAL '1 day' WHERE id = r.id;
    END LOOP;

    -- Fix calendar_item_days
    FOR r IN
        SELECT cd.id, cd.day_date
        FROM calendar_item_days cd
        JOIN calendar_items ci ON ci.id = cd.calendar_item_id
        WHERE ci.scheduled_date IS NOT NULL
          AND (
              SELECT MIN(day_date) FROM calendar_item_days
              WHERE calendar_item_id = ci.id
          ) = ci.scheduled_date - INTERVAL '1 day'
        ORDER BY cd.day_date DESC
    LOOP
        UPDATE calendar_item_days SET day_date = r.day_date + INTERVAL '1 day' WHERE id = r.id;
    END LOOP;
END;
$$;
