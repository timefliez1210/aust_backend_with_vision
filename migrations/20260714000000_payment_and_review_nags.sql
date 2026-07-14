-- Payment + review reminders: let the assistant nag about them, and let the
-- Rechnungsausgangsbuch mark storage invoices paid.
--
-- Three additive changes:
--   1. storage_invoices.paid_at  — the register's "Bezahlt" column had no column
--      to read for Lagerung rows, so it always rendered "—" even at status='paid'.
--   2. agent_reminders.recur_hours — recurring reminders were hard-coded to a 3h
--      cadence in the fire loop. A dunning/review nag must not ping every 3 hours;
--      it wants ~daily. Existing rows keep the old cadence via the default.
--   3. agent_reminders.source — widen the CHECK so reminders can be sourced from
--      an unpaid invoice or a due review request, not just email/manual.

-- 1 ────────────────────────────────────────────────────────────────────────────
-- Guarded on the table's existence: storage_contracts/storage_invoices arrive in
-- their own migration, and this one must not become a hard dependency on it.
DO $$
BEGIN
    IF to_regclass('public.storage_invoices') IS NOT NULL THEN
        ALTER TABLE storage_invoices ADD COLUMN IF NOT EXISTS paid_at TIMESTAMPTZ;

        -- Backfill: rows already marked paid get their best-known payment date so
        -- the register doesn't show a blank "Bezahlt" cell for historical entries.
        UPDATE storage_invoices
        SET paid_at = COALESCE(sent_at, created_at)
        WHERE status = 'paid' AND paid_at IS NULL;
    END IF;
END $$;

-- 2 ────────────────────────────────────────────────────────────────────────────
ALTER TABLE agent_reminders
    ADD COLUMN IF NOT EXISTS recur_hours INTEGER NOT NULL DEFAULT 3
        CHECK (recur_hours BETWEEN 1 AND 168);

-- 3 ────────────────────────────────────────────────────────────────────────────
-- Widening a CHECK: every existing row already satisfies the new predicate
-- (it is a strict superset), so no backfill is needed before re-adding it.
ALTER TABLE agent_reminders
    DROP CONSTRAINT IF EXISTS agent_reminders_source_check;

ALTER TABLE agent_reminders
    ADD CONSTRAINT agent_reminders_source_check
        CHECK (source IN ('manual', 'email', 'invoice', 'review'));
