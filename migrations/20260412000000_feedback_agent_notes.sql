-- Extend feedback_reports for agent use:
--   1. agent_notes TEXT — written by Claude/agents to document what was fixed or what clarification is needed
--   2. needs_clarification status — agent flags a report when it cannot proceed without more info

ALTER TABLE feedback_reports
    ADD COLUMN IF NOT EXISTS agent_notes TEXT;

ALTER TABLE feedback_reports
    DROP CONSTRAINT IF EXISTS feedback_reports_status_check;

ALTER TABLE feedback_reports
    ADD CONSTRAINT feedback_reports_status_check
        CHECK (status IN ('open', 'in_progress', 'resolved', 'needs_clarification'));
