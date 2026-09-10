-- Cap how many times a login code may be guessed.
--
-- Verification had no attempt counter at all: request one code for an address, then
-- grind the six-digit space against the verify endpoint for the ten minutes it lives.
-- Success hands out a 30-day session with that person's inquiries, addresses, offers
-- and PDFs. The same shape applies to the worker portal.
--
-- The counter lives on the code rather than on the address, so the lockout expires
-- with the code itself and cannot be used to lock a real person out for longer than
-- their own code would have lived. Failing five times kills every code currently
-- live for that address; the next step is to request a new one.

ALTER TABLE customer_otps ADD COLUMN IF NOT EXISTS attempts INT NOT NULL DEFAULT 0;
ALTER TABLE employee_otps ADD COLUMN IF NOT EXISTS attempts INT NOT NULL DEFAULT 0;

COMMENT ON COLUMN customer_otps.attempts IS
'Failed verification attempts against this code. At MAX_OTP_ATTEMPTS the code stops being valid; see crates/api/src/services/otp_service.rs.';
COMMENT ON COLUMN employee_otps.attempts IS
'Failed verification attempts against this code. At MAX_OTP_ATTEMPTS the code stops being valid; see crates/api/src/services/otp_service.rs.';
