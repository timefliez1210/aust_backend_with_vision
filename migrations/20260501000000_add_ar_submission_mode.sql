-- Add 'ar' to submission_mode CHECK constraint to allow AR submissions.
-- The AR endpoint creates inquiries with submission_mode = 'ar' which was
-- missing from the original constraint, causing INSERT failures.

ALTER TABLE inquiries DROP CONSTRAINT IF EXISTS inquiries_submission_mode_check;
ALTER TABLE inquiries ADD CONSTRAINT inquiries_submission_mode_check
    CHECK (submission_mode IN ('termin', 'manuell', 'foto', 'video', 'ar', 'mobile'));