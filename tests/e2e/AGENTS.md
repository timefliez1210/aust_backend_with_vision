# tests/e2e — Playwright End-to-End Suite

Browser-level tests that drive the **admin dashboard** and **worker portal** against
a real backend (Axum → Postgres → MinIO → Mailpit). No mocks: data is seeded
through the actual REST API, OTP codes are read out of Mailpit, and assertions
run against the rendered SPA.

## The canonical way to run it: `scripts/staging.sh`

**Do not hand-roll the stack.** The whole environment is scripted. From the repo root:

```bash
scripts/staging.sh up      # build + start the full stack in Docker, wait for health
scripts/staging.sh test    # backend-unit + frontend-unit + integration + Playwright e2e
scripts/staging.sh down     # stop (volumes preserved)
scripts/staging.sh clean    # stop + DELETE volumes (full reset)
```

`staging.sh up` brings up `docker/docker-compose.staging.yml`:

| Service            | Port (host→container) | Notes |
|--------------------|-----------------------|-------|
| `staging-backend`  | **8099** → 8080       | built from `docker/Dockerfile.backend` |
| `staging-frontend` | **4173** → 80         | built from `docker/Dockerfile.frontend` with build-arg `VITE_API_BASE=http://localhost:8099` baked in |
| `staging-postgres` | 5435 → 5432           | `pgvector/pgvector:pg16`; **persistent volume** |
| `staging-minio`    | 9010/9011             | S3 |
| `staging-mailpit`  | 1025 (SMTP) / 8025 (UI) | catches all outbound email (OTP codes) |

`staging.sh test` runs the Playwright leg with `STAGING_URL=http://localhost:8099`
and `FRONTEND_URL=http://localhost:4173` exported, after `npm ci` in this dir.

> The two ports matter: the **backend is :8099** and the **frontend is built to talk
> to :8099**. Pointing tests at the host debug backend on :8080 works *only* because it
> shares the same staging Postgres/MinIO/Mailpit volumes — it is not the canonical target.

## Admin seeding (required for admin login)

The admin specs log in as `admin@integration-test.invalid` /
`integration-test-password-1234` via `POST /auth/login`. That user is **not**
auto-seeded by the backend. It is upserted (argon2) into staging Postgres by the
**vitest integration global setup**: `frontend/tests/integration/globalSetup.ts`,
run via `npm run test:integration` (from `frontend/`). Because the staging
Postgres volume is persistent, one run seeds it for all later Playwright runs.

If admin login 401s on a fresh DB, run the integration suite once (or re-run its
globalSetup) to seed — don't write a new ad-hoc seed script.

## Running a single spec during development

With the stack already up (`staging.sh up`), from this dir:

```bash
STAGING_URL=http://localhost:8099 FRONTEND_URL=http://localhost:4173 \
  npx playwright test admin-payroll-hours.spec.ts --project=chromium --reporter=list
```

- `--project=chromium` for admin (desktop) specs; `--project=mobile` (Pixel 5,
  touch) for worker-portal specs — the touch emulation is what surfaces tap-only
  bugs (see `worker-termin.spec.ts`).
- Defaults if the env vars are unset: backend `:8080`, frontend `:4173` (see
  `tests/worker-helpers.ts` → `API_BASE`, and `FRONT` in each spec). These exist
  so a host-side `run-test-backend.sh` + `vite preview` loop works for quick
  iteration, but CI uses the :8099 stack above.

## Layout & conventions

- `tests/worker-helpers.ts` — API seeding helpers (admin token, create
  customer/employee/inquiry, assign crew, set per-day hours, worker OTP login via
  Mailpit) + browser auth injection (`injectAdminAuth`, `injectWorkerAuth`). **Add
  new seeding helpers here**, not inline in specs.
- `tests/helpers.ts` — a pre-generated admin `TEST_JWT` (HS256, staging secret)
  and `injectAuth` for specs that only need a client-side session.
- Each spec seeds its own data in `beforeAll` and tears it down in `afterAll`
  (hard-delete inquiries/employees/customers under `integration-test.invalid`).
  Keep tests independent; use `test.describe.configure({ mode: 'serial' })` only
  when a later test genuinely consumes an earlier one's persisted state (e.g.
  `admin-payroll-hours.spec.ts`: edit → then säubern).
- **Multi-day appointments** are historically the fragile case — cover them.
  Build one by creating an inquiry, `PATCH`ing `end_date` to stretch the range,
  **then** assigning the employee (the assignment fans out one
  `inquiry_employees` row per day in `scheduled_date..=end_date`).
- Pin `timezoneId: 'Europe/Berlin'` so the HH:MM a worker types maps back to the
  HH:MM the admin table renders.

## Specs

| Spec | Project | Covers |
|------|---------|--------|
| `auth.spec.ts`, `dashboard.spec.ts`, `quotes.spec.ts`, `offers.spec.ts` | chromium | admin login + core dashboard pages |
| `worker-job-hours.spec.ts`, `worker-pending-hours.spec.ts` | mobile | worker logs hours; loose keypad input; admin read-back |
| `worker-termin.spec.ts` | mobile | calendar-Termin job cards are tappable + show addresses |
| `admin-payroll-hours.spec.ts` | chromium | Stundenkonto: multi-day deactivate + paid-time adjust, live hour-account math, persistence, **destructive "Stundenkonto säubern"** cleanup, edit-gate |

## Known environment caveat: Node 24 + Playwright loader

Playwright 1.61's sync ESM loader calls `context.conditions?.includes("import")`
(`node_modules/playwright/lib/common/index.js`). Under **Node ≥24**, `context.conditions`
is no longer a plain array, so this throws `context.conditions?.includes is not a
function` while *collecting any spec that imports a local module* — the whole suite
fails before a single test runs.

Fix at the repo level (so `npm ci` doesn't wipe it):
- pin Node ≤22 for this package (`.nvmrc` / `engines`), **or**
- bump the Playwright pin in `package.json` to a version that resolves this.

A local edit to the bundled loader works for a one-off run but is `.gitignore`d
under `node_modules/` and is erased by the `npm ci` inside `staging.sh test`.
