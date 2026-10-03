-- Gewinn-Tab: Alex's personal cost accounting (controlling, not tax bookkeeping).
--
-- Built GoBD-ready but not GoBD-enforced: a correction is a Storno row by default
-- (`storno_of`), hard deletes are still allowed, and every write lands in
-- `accounting_audit_log` — which carries no FK so it outlives the rows it describes.
-- `skr03_account` and `locked_at` are prepared for real bookkeeping and unused today.

CREATE TABLE expense_categories (
    id               UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    name             TEXT        NOT NULL UNIQUE,
    -- fixed = Fixkosten (Miete, Versicherung, Kredit), variable = per-job/usage
    -- (Diesel, Reparatur, Material), wages = Löhne (feeds the real hourly rate).
    kind             TEXT        NOT NULL CHECK (kind IN ('fixed', 'variable', 'wages')),
    default_vat_rate SMALLINT    NOT NULL DEFAULT 19 CHECK (default_vat_rate IN (0, 7, 19)),
    skr03_account    TEXT,
    sort_order       INTEGER     NOT NULL DEFAULT 0,
    active           BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO expense_categories (name, kind, default_vat_rate, sort_order) VALUES
    ('Miete',                   'fixed',    19, 10),
    ('Versicherung',            'fixed',     0, 20),
    ('Kfz-Steuer',              'fixed',     0, 30),
    ('Finanzierung / Kredit',   'fixed',     0, 40),
    ('Telefon & Internet',      'fixed',    19, 50),
    ('Software & Abos',         'fixed',    19, 60),
    ('Löhne',                   'wages',     0, 100),
    ('Kraftstoff',              'variable', 19, 200),
    ('Reparatur & Wartung',     'variable', 19, 210),
    ('Material & Verpackung',   'variable', 19, 220),
    ('Fremdleistungen',         'variable', 19, 230),
    ('Gebühren',                'variable',  0, 240),
    ('Werbung',                 'variable', 19, 250),
    ('Bürobedarf',              'variable', 19, 260),
    ('Sonstiges',               'variable', 19, 900);

CREATE TABLE recurring_expenses (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    category_id     UUID        NOT NULL REFERENCES expense_categories(id),
    label           TEXT        NOT NULL,
    supplier        TEXT,
    netto_cents     BIGINT      NOT NULL,
    vat_rate        SMALLINT    NOT NULL CHECK (vat_rate IN (0, 7, 19)),
    vat_cents       BIGINT      NOT NULL,
    brutto_cents    BIGINT      NOT NULL,
    interval_months SMALLINT    NOT NULL DEFAULT 1 CHECK (interval_months IN (1, 3, 6, 12)),
    -- Day the payment is due; clamped to 28 so it exists in every month.
    day_of_month    SMALLINT    NOT NULL DEFAULT 1 CHECK (day_of_month BETWEEN 1 AND 28),
    -- First day of the first month that is charged / of the last one (NULL = open-ended).
    start_month     DATE        NOT NULL,
    end_month       DATE,
    vehicle_id      UUID        REFERENCES vehicles(id) ON DELETE SET NULL,
    active          BOOLEAN     NOT NULL DEFAULT TRUE,
    notes           TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE expenses (
    id               UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    category_id      UUID        NOT NULL REFERENCES expense_categories(id),
    -- 'draft' = generated from a recurring template, waits for Alex to confirm;
    -- only 'booked' rows count as actual cost.
    status           TEXT        NOT NULL DEFAULT 'booked' CHECK (status IN ('draft', 'booked')),
    receipt_date     DATE        NOT NULL,              -- Belegdatum
    paid_on          DATE,                              -- Zahldatum
    -- First day of the month the cost belongs to (rent for October paid on 28.09.,
    -- September wages paid in October). Defaults to the receipt month.
    period_month     DATE        NOT NULL CHECK (EXTRACT(DAY FROM period_month) = 1),
    supplier         TEXT,
    receipt_number   TEXT,
    description      TEXT,
    netto_cents      BIGINT      NOT NULL,
    vat_rate         SMALLINT    NOT NULL CHECK (vat_rate IN (0, 7, 19)),
    vat_cents        BIGINT      NOT NULL,
    brutto_cents     BIGINT      NOT NULL,
    vehicle_id       UUID        REFERENCES vehicles(id) ON DELETE SET NULL,
    inquiry_id       UUID        REFERENCES inquiries(id) ON DELETE SET NULL,
    employee_id      UUID        REFERENCES employees(id) ON DELETE SET NULL,
    recurring_id     UUID        REFERENCES recurring_expenses(id) ON DELETE SET NULL,
    receipt_s3_key   TEXT,
    receipt_filename TEXT,
    receipt_mime     TEXT,
    receipt_sha256   TEXT,
    -- Set on a Storno row: the booking it reverses (amounts are negated).
    storno_of        UUID        REFERENCES expenses(id) ON DELETE SET NULL,
    locked_at        TIMESTAMPTZ,
    created_by       TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_expenses_period    ON expenses (period_month);
CREATE INDEX idx_expenses_inquiry   ON expenses (inquiry_id) WHERE inquiry_id IS NOT NULL;
CREATE INDEX idx_expenses_vehicle   ON expenses (vehicle_id) WHERE vehicle_id IS NOT NULL;
CREATE INDEX idx_expenses_employee  ON expenses (employee_id) WHERE employee_id IS NOT NULL;
-- A booking can be reversed once.
CREATE UNIQUE INDEX uq_expenses_storno_of ON expenses (storno_of) WHERE storno_of IS NOT NULL;
-- A recurring template produces at most one entry per month.
CREATE UNIQUE INDEX uq_expenses_recurring_period
    ON expenses (recurring_id, period_month) WHERE recurring_id IS NOT NULL;

-- Monthly hours transfer ("Stunden für Monat X übernehmen"): a frozen snapshot of
-- each employee's paid hours, so later edits in the hours tab never silently
-- change a closed month. Re-transferring overwrites the row (logged).
CREATE TABLE labor_months (
    id                UUID          PRIMARY KEY DEFAULT gen_random_uuid(),
    employee_id       UUID          NOT NULL REFERENCES employees(id) ON DELETE RESTRICT,
    month             DATE          NOT NULL CHECK (EXTRACT(DAY FROM month) = 1),
    paid_hours        NUMERIC(8, 2) NOT NULL,
    worked_hours      NUMERIC(8, 2) NOT NULL,
    -- €/h used to cost the snapshot (the employee's real rate at transfer time,
    -- else the default) and the resulting cost.
    rate_cents        INTEGER       NOT NULL,
    cost_cents        BIGINT        NOT NULL,
    unconfirmed_days  INTEGER       NOT NULL DEFAULT 0,
    -- Per-entry paid hours (kind, source id, inquiry id, date, hours) — lets the
    -- per-job margin use the frozen hours of a transferred month.
    breakdown         JSONB         NOT NULL DEFAULT '[]'::jsonb,
    transferred_at    TIMESTAMPTZ   NOT NULL DEFAULT NOW(),
    transferred_by    TEXT,
    UNIQUE (employee_id, month)
);

CREATE INDEX idx_labor_months_month ON labor_months (month);

CREATE TABLE accounting_audit_log (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    entity      TEXT        NOT NULL,   -- expense | recurring_expense | labor_month | setting
    entity_id   UUID,
    action      TEXT        NOT NULL,   -- create | update | storno | delete | confirm | transfer
    actor       TEXT,
    before      JSONB,
    after       JSONB,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_accounting_audit_entity ON accounting_audit_log (entity, entity_id);

INSERT INTO settings (key, value, updated_at)
VALUES ('labor_default_rate_cents', '1850'::jsonb, NOW())
ON CONFLICT (key) DO NOTHING;
