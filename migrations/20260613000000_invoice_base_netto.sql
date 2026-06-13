-- Persist the invoice's base netto amount (active offer price or the manual
-- price entered at creation). Previously the base was re-derived from the
-- active offer on every PDF regeneration / totals computation, which broke
-- invoices created WITHOUT an offer (manual Rechnungsbetrag): editing extras,
-- renumbering, self-heal, and list totals all failed or reported 0.
-- NULL for pre-existing rows -> callers fall back to the active offer.
ALTER TABLE invoices ADD COLUMN base_netto_cents BIGINT;
