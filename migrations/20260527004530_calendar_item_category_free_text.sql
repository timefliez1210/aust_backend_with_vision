-- Allow operators to freely choose a Termin category (label) instead of being
-- limited to the seven preset values. UI still suggests the presets via a
-- datalist; backend already passes the string through unchanged.

ALTER TABLE calendar_items
    DROP CONSTRAINT IF EXISTS calendar_items_category_check;
