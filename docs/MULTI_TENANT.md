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
| 2 | Company profile in data: names, addresses, bank, footers, mail texts, prices, depot — out of code/TOML into `tenants`; golden tests prove identical output | no | open |
| 3 | Row-level security: app connects as a non-superuser role, `FORCE ROW LEVEL SECURITY` + policy on every table; login and session lookups through `SECURITY DEFINER` functions; two-tenant leak test over every endpoint | no | open |
| 4 | Per-tenant integrations: IMAP/SMTP mailbox, Telegram bot + bindings, Josie memory, invoice counters, S3 prefix `tenants/{id}/`, XLSX templates, background jobs per tenant | no | open |
| 5 | Per-company uniqueness: `customers.email`, `employees.email`, `invoices.invoice_number`, `storage_invoices.invoice_number`, `expense_categories.name`, `calendar_capacity_overrides.override_date`, `settings.key`, `invoice_number_counters.year` become unique per tenant | no | open — drops the old indexes, needs an exception to "migrations additive-only" |
| 6 | Onboarding: create a tenant + first admin; console reads name/accent from the API (`lib/tenant.ts`); console on its own host, `aust-umzuege.de` stays the marketing site | no | open |

### Known gaps until step 3

- The app's database role (`aust`) is a superuser. Superusers bypass row-level
  security even with `FORCE`, so step 3 needs a new non-superuser role that owns
  the tables (one-time ops change on the VPS).
- Login (`users` by email) and session lookups run before the tenant is known;
  under RLS they need `SECURITY DEFINER` functions or an exemption.
- 46 `tokio::spawn` sites in request and job code still use the plain spawn.
