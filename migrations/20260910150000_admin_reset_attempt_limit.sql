-- Cap how many times an admin password-reset code may be guessed.
--
-- The reset code is six digits and lives fifteen minutes, and verification had no
-- attempt counter: grind it for a known administrator address and set an arbitrary
-- new password. That is full takeover of the dashboard with no credential to start
-- from, so this is the most valuable of the guessable codes in the system.
--
-- Same shape as customer_otps.attempts: the count sits on the code, so the lockout
-- expires with it and nobody can be locked out of their own reset for longer than the
-- code would have lasted.

ALTER TABLE admin_password_resets ADD COLUMN IF NOT EXISTS attempts INT NOT NULL DEFAULT 0;

COMMENT ON COLUMN admin_password_resets.attempts IS
'Failed verification attempts against this code. At MAX_OTP_ATTEMPTS the code stops being valid; see crates/api/src/routes/auth.rs.';
