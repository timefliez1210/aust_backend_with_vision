-- OTP codes used for employee magic-link login.
-- Mirrors the customer_otps pattern.
CREATE TABLE employee_otps (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    email      VARCHAR(255) NOT NULL,
    code       VARCHAR(6)   NOT NULL,
    expires_at TIMESTAMPTZ  NOT NULL,
    used       BOOLEAN      NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_employee_otps_email ON employee_otps(email);

-- Long-lived DB-backed sessions for authenticated employees.
-- Mirrors the customer_sessions pattern.
CREATE TABLE employee_sessions (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    employee_id UUID        NOT NULL REFERENCES employees(id) ON DELETE CASCADE,
    token       VARCHAR(64) NOT NULL UNIQUE,
    expires_at  TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_employee_sessions_token ON employee_sessions(token);
