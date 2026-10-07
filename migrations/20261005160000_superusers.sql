-- Platform superusers: may see and create companies (tenants).
--
-- Additive only. Set exclusively from the server's command line
-- (`aust_backend superuser <email> on|off`) — there is no API that changes it,
-- so only whoever runs the server can grant it. See docs/MULTI_TENANT.md.

ALTER TABLE users ADD COLUMN is_superuser BOOLEAN NOT NULL DEFAULT false;
