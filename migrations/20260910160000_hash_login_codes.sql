-- Store login codes as Argon2 hashes instead of plaintext.
--
-- customer_otps.code and employee_otps.code held the six digits verbatim, so a
-- read-only replica, a nightly backup or a dump was a list of working login codes for
-- every address in the table. The admin password reset already hashed its code; these
-- two did not.
--
-- The column has to grow: an Argon2 PHC string is roughly 95 characters, not 6.
--
-- Existing rows are left alone rather than migrated — a six-digit value simply stops
-- verifying. Codes live ten minutes, so the only effect is that anyone mid-login when
-- this deploys requests a new one.

ALTER TABLE customer_otps ALTER COLUMN code TYPE TEXT;
ALTER TABLE employee_otps ALTER COLUMN code TYPE TEXT;

COMMENT ON COLUMN customer_otps.code IS
'Argon2 hash of the six-digit login code. Never the plaintext; salted, so it is verified by the service rather than looked up by equality.';
COMMENT ON COLUMN employee_otps.code IS
'Argon2 hash of the six-digit login code. Never the plaintext; salted, so it is verified by the service rather than looked up by equality.';
