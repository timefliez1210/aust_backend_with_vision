-- Months a Dauerauftrag must NOT generate again because Alex deleted that month's
-- entry on purpose. Replaces the old "never backfill before the newest entry" rule,
-- which also blocked legitimate backfills: a template created with start October
-- and then moved to January never produced January–September.
CREATE TABLE recurring_expense_skips (
    recurring_id UUID        NOT NULL REFERENCES recurring_expenses(id) ON DELETE CASCADE,
    period_month DATE        NOT NULL CHECK (EXTRACT(DAY FROM period_month) = 1),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (recurring_id, period_month)
);
