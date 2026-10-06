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
  - public endpoints (forms, flash contact, OTP login, password reset): the
    `Origin`/`Referer` host, looked up in `tenants.domains`
    (`middleware::scope_by_origin`); an unlisted host stays Aust.

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
| 5 | Per-company uniqueness (exception to additive-only, approved 2026-10-05): `customers.email` (among unmerged rows), `invoices`/`storage_invoices.invoice_number`, `expense_categories.name`, `calendar_capacity_overrides.override_date`, `settings` and `invoice_number_counters` keys are unique per tenant; KVA numbers: Aust keeps `offer_number_seq`, others count in `offer_number_counters`. The migration merges prod's two exact duplicate customers into the older row like the app's merge does. `employees.email` and `users.email` stay globally unique — they identify the tenant at login | no | done |
| 6 | Public requests by domain (`tenants.domains`, `Origin`/`Referer` → scope; CORS allows those hosts); console branding from `GET /api/v1/tenant` (`lib/tenant.svelte.ts`, Aust defaults, no flash); company profile + templates via `/api/v1/admin/tenant`; `aust_backend tenant-create <slug> <name> <admin-email>` | no | done — hosting the console on its own domain is a deploy decision |

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
`scripts/dev-up.sh` runs the same script and connects as `aust_app`, so local
click-throughs see the separation exactly as prod will.

## Onboarding a company

**Platform superusers** see the console tab "Firmen" (`/admin/platform`): every
company with a few counts, and a form that creates a company plus its first admin
and shows that admin's one-time password once. The flag `users.is_superuser` is
set only from the server — `aust_backend superuser <email> on|off` — never through
the API; the platform routes re-check it in the database on every call (the
token's `su` claim only shows the tab). The steps below are the same, whether the
company is created there or with `tenant-create`.

1. Prod must enforce row-level security first (role switch above) — the backend
   refuses to start with two tenants otherwise.
2. In the console tab "Firmen" (superusers), or
   `docker exec aust_backend aust_backend tenant-create <slug> "<Name>" <admin-email>`
   — either shows the admin's one-time password.
3. Set `tenants.domains` (its website / console hosts) and the profile
   (`PUT /api/v1/admin/tenant` as that admin: names, phone, depot, accent, persona).
4. Mailbox and bot: `[tenants.<slug>]` with `email` and `telegram` sections (env
   `AUST__TENANTS__<SLUG>__EMAIL__…`). Without them the company simply has no
   mailbox / bot. Flash-contact sidecar: one more container with
   `AUST__TENANT_ID=<id>` and its own bot token.
5. Documents work immediately: the company's KVA, invoice and travel-expense
   templates are Aust's layout with its letterhead (name, address, contact, bank,
   tax numbers), logo and accent colour swapped in (`offer-generator/src/letterhead.rs`;
   a test checks no trace of Aust survives). Optional: upload its own template via
   `PUT /api/v1/admin/tenant/templates/{offer|invoice|travel_expense|clearing_page_2}`
   — an upload wins. A clearing KVA keeps the derived page 2 unless one is uploaded.
6. Restart the backend (tenants, domains and slugs are read at startup).

## Guarantees added after review (2026-10-07)

- Creating a company (API "Firmen" and `tenant-create`) is refused while the
  database role bypasses row-level security — the role switch comes first.
- Migrations run on their own connection with the RLS bypass open, so a backfill
  reaches every company's rows. `CREATE EXTENSION` needs a superuser: after the
  role switch, create new extensions by hand before deploying the migration.
- Links between tenant tables cannot cross companies: every single-column foreign
  key onto a parent's `id` has a twin on `(tenant_id, column)`
  (`20261007120000_tenant_review_fixes.sql`; `rls_tests` fails for a new link
  without one).
- Daily-briefing slots, Telegram sessions and chat bindings are per company (one
  person can talk to several companies' bots).
- Password reset and worker code login find the person by email across companies
  (email is unique system-wide for users and employees), then continue inside
  that person's company. Customer login stays per domain (customer emails are
  per company).

## Known gaps

- Unscoped code (no token, no listed `Origin`) still counts as Aust:
  `current_tenant_id()` falls back to Aust. That keeps every existing caller
  working; a request from an unlisted host lands with Aust, as today.
- The customer app (`capacitor://localhost`) is Aust's app; another company's
  app would need its own build with a tenant header.
- `GET /api/v1/estimates/images/{*key}` serves stored files without login (as
  before multi-tenancy); keys contain UUIDs, a per-tenant prefix would close it.
- Storage keys have no tenant prefix. Objects are only reached through their
  row (which RLS guards), and keys carry row UUIDs, so tenants cannot collide;
  a per-tenant prefix would only help bulk export/deletion.
- New tenants need a backend restart (tenants, slugs and templates are read at
  startup).
