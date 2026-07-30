-- Promote inquiry_appointments from a lightweight Besichtigung marker into a
-- full work entry that can carry a crew with hours (e.g. Halteverbotszonen-Aufbau,
-- which is paid labour for the employees). The entry stays linked to its inquiry
-- and stays a single dated entry — multiple visits are still multiple rows.
--
-- Additive-only. Existing lightweight appointments keep working: the new columns
-- are nullable and the crew junction is empty until an employee is assigned.

-- ── Extra fields on the appointment itself ──────────────────────────────────
ALTER TABLE inquiry_appointments
    ADD COLUMN IF NOT EXISTS description    TEXT,
    -- Structured own-address (free-text `location` kept as a fallback).
    ADD COLUMN IF NOT EXISTS address_id     UUID REFERENCES addresses(id) ON DELETE SET NULL,
    -- Admin note shown to every assigned crew member (mirrors calendar_items.employee_notes).
    ADD COLUMN IF NOT EXISTS employee_notes TEXT;

-- ── Crew junction ───────────────────────────────────────────────────────────
-- Columns mirror calendar_item_employees (unified time fields from
-- 20260413120000 + travel-expense fields from 20260423000000/1), MINUS job_date:
-- an appointment is exactly one day, so there is one row per (appointment, employee).
CREATE TABLE IF NOT EXISTS inquiry_appointment_employees (
    id                    UUID          PRIMARY KEY DEFAULT gen_random_uuid(),
    appointment_id        UUID          NOT NULL REFERENCES inquiry_appointments(id) ON DELETE CASCADE,
    employee_id           UUID          NOT NULL REFERENCES employees(id)            ON DELETE CASCADE,
    -- Planned + admin-recorded time tracking
    planned_hours         NUMERIC,
    start_time            TIME,
    end_time              TIME,
    break_minutes         INT           NOT NULL DEFAULT 0,
    actual_hours          NUMERIC(5,2),
    clock_in              TIME,
    clock_out             TIME,
    -- Worker self-reported times (TIMESTAMPTZ, same as the other junctions)
    employee_clock_in     TIMESTAMPTZ,
    employee_clock_out    TIMESTAMPTZ,
    employee_break_minutes INT,
    notes                 TEXT,
    -- Travel-expense / Verpflegungspauschale fields (parity with the other junctions)
    transport_mode        VARCHAR(50),
    travel_costs_cents    BIGINT,
    accommodation_cents   BIGINT,
    misc_costs_cents      BIGINT,
    meal_deduction        VARCHAR(50),
    created_at            TIMESTAMPTZ   NOT NULL DEFAULT NOW(),
    UNIQUE (appointment_id, employee_id)
);

CREATE INDEX IF NOT EXISTS idx_inquiry_appointment_employees_appt
    ON inquiry_appointment_employees(appointment_id);
CREATE INDEX IF NOT EXISTS idx_inquiry_appointment_employees_emp
    ON inquiry_appointment_employees(employee_id);
