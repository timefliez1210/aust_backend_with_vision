-- Seed admin user for dashboard access
-- This hash is a placeholder that will NOT verify any password.
-- After running migrations, generate a real password hash and update:
--   UPDATE users SET password_hash = '<new_argon2_hash>' WHERE email = 'admin@aust-umzuege.de';
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
