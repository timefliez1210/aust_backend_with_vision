-- Seed admin user for dashboard access
-- Default password: REDACTED_DEFAULT_PASSWORD (CHANGE IMMEDIATELY after first login)
INSERT INTO users (id, email, password_hash, name, role, created_at, updated_at)
VALUES (
    gen_random_uuid(),
    'admin@aust-umzuege.de',
    '$argon2id$v=19$m=65536,t=3,p=4$PLACEHOLDER_SALT_000$PLACEHOLDER_HASH_WILL_NOT_VERIFY',
    'Alex',
    'admin',
    NOW(),
    NOW()
)
ON CONFLICT (email) DO NOTHING;
