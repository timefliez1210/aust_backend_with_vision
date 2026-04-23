-- Add misc_costs_cents to employee assignment tables
-- Used for Reisekostenabrechnung "Sonstige Kosten / Reisenebenkosten"

ALTER TABLE inquiry_employees
    ADD COLUMN misc_costs_cents BIGINT;

ALTER TABLE calendar_item_employees
    ADD COLUMN misc_costs_cents BIGINT;
