-- Simplify flash_contact time_preference to three plain values matching the UI labels.
-- Old: any_time, 08-10, 10-12, 14-16, 16-18
-- New: gleich, vormittag, nachmittag

ALTER TABLE flash_contacts DROP CONSTRAINT IF EXISTS flash_contacts_time_preference_check;

UPDATE flash_contacts SET time_preference = 'gleich'     WHERE time_preference = 'any_time';
UPDATE flash_contacts SET time_preference = 'vormittag'  WHERE time_preference IN ('08-10', '10-12');
UPDATE flash_contacts SET time_preference = 'nachmittag' WHERE time_preference IN ('14-16', '16-18');

ALTER TABLE flash_contacts ADD CONSTRAINT flash_contacts_time_preference_check
    CHECK (time_preference IN ('gleich', 'vormittag', 'nachmittag'));
