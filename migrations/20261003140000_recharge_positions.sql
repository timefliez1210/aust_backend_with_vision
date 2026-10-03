-- Weiterberechnete Kosten: a cost category can be recharged to customers through
-- KVA/invoice positions (matched by position name, case-insensitive). The Gewinn tab
-- then compares what came in through those positions with what the category cost —
-- pass-through, profitable or loss-making is computed, never declared.
--
-- A recharged category is not loaded onto the hourly rate; `in_hourly_rate` is kept
-- in sync (= no positions) so the Stundensatz-Kalkulation reads one column.
ALTER TABLE expense_categories
    ADD COLUMN IF NOT EXISTS recharge_positions TEXT[] NOT NULL DEFAULT '{}';

INSERT INTO expense_categories (name, kind, default_vat_rate, sort_order)
VALUES ('Kartons', 'variable', 19, 225)
ON CONFLICT (name) DO NOTHING;

UPDATE expense_categories SET recharge_positions = ARRAY['Fahrkostenpauschale']
    WHERE name = 'Kraftstoff';
UPDATE expense_categories SET recharge_positions = ARRAY['Umzugsmaterial']
    WHERE name = 'Material & Verpackung';
UPDATE expense_categories SET recharge_positions = ARRAY['Halteverbotszone']
    WHERE name = 'Gebühren';
UPDATE expense_categories
    SET recharge_positions = ARRAY['Verkauf U-Karton', 'Verkauf B-Karton', 'Verkauf Seidenpapier', 'Fernsehkarton']
    WHERE name = 'Kartons';

UPDATE expense_categories SET in_hourly_rate = (cardinality(recharge_positions) = 0);
