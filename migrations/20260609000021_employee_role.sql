-- Add a role column to employees.
--
-- Business rationale: not every employee has a user account (helpers hired per-job
-- often have no login). Storing role on employees (not users) lets us classify all
-- workers uniformly regardless of whether they can log in.
--
-- Allowed values: 'helper', 'driver', 'operator', 'supervisor'.
-- Default: 'helper' — the most common case for new records.

ALTER TABLE employees
    ADD COLUMN role text NOT NULL DEFAULT 'helper'
        CHECK (role IN ('helper', 'driver', 'operator', 'supervisor'));

-- aust_assistant already has SELECT on employees (migration 20260609000008).
-- Grant UPDATE so the assistant can change the role field.
GRANT UPDATE ON employees TO aust_assistant;
