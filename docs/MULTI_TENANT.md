# Multi-tenancy

Goal: several moving companies on one deployment, each seeing only its own data —
without Aust (tenant #1) noticing anything along the way. Every step below ships on
its own, keeps Aust's URLs, logins, numbers and documents identical, and can be
rolled back alone.

## Design

**One database, a `tenant_id` on every row, Postgres row-level security.**

- `tenants` holds the companies. Aust is `0190aa57-0000-7000-8000-000000000001`
  (`aust_core::tenant::AUST`).
- Every table except `_sqlx_migrations` and `tenants` has
  `tenant_id UUID NOT NULL DEFAULT current_tenant_id() REFERENCES tenants(id)`.
  New rows land in the right tenant without any `INSERT` naming the column.
- `current_tenant_id()` reads the session setting `app.tenant_id`. While Aust is the
  only tenant it falls back to Aust when the setting is unset; before a second
  tenant goes live that fallback becomes an error (one function, one switch).
- The running task's tenant is a tokio task-local (`aust_core::tenant`). Auth
  middleware wraps each request in `tenant::scope(..)`; the pool
  (`aust_api::create_pool`) copies it into `app.tenant_id` when a connection is
  opened and every time an idle one is handed out. Queries never pass the tenant
  themselves — RLS policies and column defaults read `current_tenant_id()`.
- Where the tenant comes from:
  - admins: the `tid` claim of the JWT (tokens without one count as Aust, so nobody
    is logged out);
  - workers and customers: their session row (`employee_sessions` /
    `customer_sessions`);
  - public endpoints (forms, flash contact, app submissions): not resolved yet —
    Aust. Later by host name or API key.

### Rules for new code

- Customer-facing text never names the company literally: take it from
  `TenantProfile` (`tenant_repo::profile`). Add a golden test when you touch one.
- `tokio::spawn` drops the tenant. Use `aust_core::tenant::spawn` inside a request.
- A background job that serves every tenant loops over `tenants` and runs each
  pass inside `tenant::scope`.
- A copy between tables (`INSERT … SELECT`) that can cross tenants lists
  `tenant_id` explicitly (see the `domain_events` archive sweep).
- Advisory-lock keys and anything else that is "unique per company" include the
  tenant.

## Steps

| # | Step | Visible to Aust | Status |
|---|------|-----------------|--------|
| 1 | `tenants` table, `tenant_id` everywhere, tenant context (task-local → pool → `app.tenant_id`), `tid` claim | no | done |
| 2 | Company profile in data: names, phone, review link, owner — out of code into `tenants` (`TenantProfile`, loaded per request); golden tests pin Aust's mails, prompts and exports word for word | no | done |
| 2b | Depot per tenant (`tenants.depot_address`). Prices already live per key in `settings`, with `[company]` in TOML as the default — they become per company with steps 3 and 5 | no | done |
| 3 | Row-level security: `FORCE ROW LEVEL SECURITY` + policy `tenant_isolation` on every tenant table; login and session lookups through `tenant::bypass`; the whole test suite passes as a non-superuser role | no | code done — enforced on prod only after the role switch below |
| 4 | Per-tenant integrations: mailbox + Telegram from `[tenants.<slug>]` (`Config::email()` / `telegram()` follow the running tenant; a tenant without its own gets a disabled one, never Aust's); one `EmailProcessor` + offer handler + event consumer per tenant; every periodic job runs once per tenant in its scope; `tenant::spawn` everywhere; document templates per tenant (`tenant_templates`, Aust keeps the compiled-in ones, others get an error without their own); Josie's persona per tenant (`tenants.soul_md`, neutral persona otherwise); flash-contact sidecar per tenant (`AUST__TENANT_ID`) | no | done |
| 5 | Per-company uniqueness: `customers.email`, `employees.email`, `invoices.invoice_number`, `storage_invoices.invoice_number`, `expense_categories.name`, `calendar_capacity_overrides.override_date`, `settings.key`, `invoice_number_counters.year` become unique per tenant | no | open — drops the old indexes, needs an exception to "migrations additive-only" |
| 6 | Onboarding: create a tenant + first admin; console reads name/accent from the API (`lib/tenant.ts`); console on its own host, `aust-umzuege.de` stays the marketing site | no | open |

## Prod: switch to a non-superuser role (enforces step 3)

Superusers skip row-level security, and prod connects as the superuser `aust`.
Until this runs, the policies exist but change nothing.

1. Backup (`/opt/aust/backup.sh`).
2. `docker exec -i aust_postgres psql -U aust -d aust_backend -v app_password="'<new password>'" < scripts/db-app-role.sql`
   — creates `aust_app` (no superuser, no BYPASSRLS) and hands it every table,
   sequence, function and type in `public`. The output must list no table "not
   owned by aust_app".
3. In `/opt/aust/.env`, point the backend's database URL at
   `aust_app:<password>`; restart the backend. Migrations keep running at
   startup — `aust_app` owns the schema.
4. Rollback: point the URL back at `aust`.

`backup.sh` keeps dumping as `aust` (superuser dumps see every row).

Tested: `scripts/db-app-role.sql` on a superuser-owned copy of the schema; the
full workspace suite against a database owned by a non-superuser role.

## Known gaps

- Pre-login flows other than admin login and session tokens — OTP request and
  verify, password reset — still run unscoped, i.e. as Aust. Fine while Aust is
  the only tenant; they need the tenant from the host name (step 6) before the
  Aust fallback in `current_tenant_id()` is removed.
- Storage keys have no tenant prefix. Objects are only reached through their
  row (which RLS guards), and keys carry row UUIDs, so tenants cannot collide;
  a per-tenant prefix would only help bulk export/deletion.
- New tenants need a backend restart (tenants, slugs and templates are read at
  startup).
- 46 `tokio::spawn` sites in request and job code still use the plain spawn.
