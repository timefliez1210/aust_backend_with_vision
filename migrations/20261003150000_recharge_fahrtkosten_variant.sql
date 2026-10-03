-- Alex's manual invoices spell it "Fahrtkostenpauschale" (with t), the KVA template
-- "Fahrkostenpauschale". Position matching is by name, so Kraftstoff needs both.
UPDATE expense_categories
SET recharge_positions = array_append(recharge_positions, 'Fahrtkostenpauschale')
WHERE name = 'Kraftstoff'
  AND 'Fahrkostenpauschale' = ANY (recharge_positions)
  AND NOT ('Fahrtkostenpauschale' = ANY (recharge_positions));
