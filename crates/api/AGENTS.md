# crates/api — REST API, Repos, Services

The main backend crate. Axum HTTP server with JWT middleware, 18 route files, 13 repository modules, 9 service modules.

## File Map

### Routes (`src/routes/`)

| File | Purpose | Size |
|------|---------|------|
| `submissions.rs` | Public form submissions (photo, mobile, AR, video, manual) | 84KB |
| `admin.rs` | Dashboard, employees, notes, feedback, timesheets | 51KB |
| `invoices.rs` | Invoice CRUD + XLSX generation | 32KB |
| `inquiry_actions.rs` | Estimation triggers, offer generation, employee assignments | 28KB |
| `inquiries.rs` | Inquiry CRUD, status transitions, PDF download, delete | 20KB |
| `calendar.rs` | Calendar schedule, availability, bookings | 25KB |
| `calendar_items.rs` | Calendar item CRUD (non-inquiry work blocks) | 18KB |
| `customer.rs` | Customer-facing endpoints (OTP auth, inquiry list) | 19KB |
| `employee.rs` | Employee CRUD, document upload/download, hours | 25KB |
| `admin_customers.rs` | Admin customer CRUD, address update | 14KB |
| `admin_emails.rs` | Email thread CRUD, drafts, send | 17KB |
| `auth.rs` | JWT login/refresh | 17KB |
| `estimates.rs` | Volume estimation CRUD + image serving | 36KB |

### Repositories (`src/repositories/`)

| File | Key Tables | Notes |
|------|-----------|-------|
| `inquiry_repo.rs` | `inquiries`, `inquiry_days`, `inquiry_day_employees` | 37KB — largest, most complex |
| `employee_repo.rs` | `employees`, `inquiry_day_employees` | Document keys use `resolve_doc_column()` allowlist |
| `admin_repo.rs` | Aggregation queries (dashboard, orders) | |
| `calendar_repo.rs` | `calendar_items`, day-employees | Single-day branch uses `inquiry_day_employees` |
| `customer_repo.rs` | `customers` | |
| `offer_repo.rs` | `offers` | Unique partial index `offers_inquiry_active_unique` prevents duplicate active offers |
| `estimation_repo.rs` | `volume_estimations` | |
| `invoice_repo.rs` | `invoices`, `invoice_line_items` | |
| `address_repo.rs` | `addresses` | |
| `customer_auth_repo.rs` | `customer_sessions`, OTP | |
| `email_repo.rs` | `email_threads`, `email_messages` | |

### Services (`src/services/`)

| File | Purpose |
|------|---------|
| `offer_builder.rs` | **71KB** — full offer generation pipeline. Calls pricing engine, builds line items, generates XLSX/PDF, inserts offer. Race-condition safe via DB unique constraint. |
| `inquiry_builder.rs` | Canonical response builder — assembles inquiry detail from 6+ repo calls |
| `telegram_service.rs` | Telegram approval bot (✅ Approve / ✏️ Edit / ❌ Deny) |
| `offer_pipeline.rs` | Auto-offer trigger: check readiness → calculate distance → generate offer |
| `email_dispatch.rs` | SMTP email sending on offer approval |
| `email.rs` | Email formatting helpers |
| `otp_service.rs` | OTP generation + verification |
| `vision.rs` | Vision service client (photo, depth, video) |

## Critical Patterns

### Repository Pattern
ALL SQL goes in `src/repositories/*_repo.rs`. Route handlers never contain inline `sqlx::query`. If you need a new query, add a function to the appropriate repo module.

### Dual-Write: `inquiry_employees` → `inquiry_day_employees`
Write paths mirror to both flat (`inquiry_employees`) and day-level (`inquiry_day_employees`) tables. Read paths use day-level as primary. The flat table still exists for backward compat but should not be used for new reads.

### Status Gate (M3)
`InquiryStatus::is_locked_for_modifications()` returns true for `offer_ready` through `paid`. When locked, PATCH `/inquiries/{id}` rejects changes to `estimated_volume_m3`, `services`, `distance_km`, `origin_address_id`, `destination_address_id`.

### Offer Race Condition (M1)
`offers_inquiry_active_unique` partial unique index prevents duplicate active offers. `offer_builder.rs::insert_returning()` catches constraint violations and falls back to updating the existing offer.

### Configurable Pricing (M2)
All pricing constants are in `CompanyConfig`:
- `rate_per_person_hour_cents` (default 3000 = €30/hr)
- `assembly_price` (default 25.0 = €25)
- `parking_ban_price` (default 100.0 = €100)
- `packing_price` (default 30.0 = €30)
- `saturday_surcharge_cents` (default 5000 = €50)
- `fahrt_rate_per_km` (default 1.0)

`PricingEngine::with_rate(rate, surcharge)` and `ServicePrices::from_config(config)` replace `PricingEngine::new()` in non-test code.

### Submission Handlers
5 handlers in `submissions.rs`: photo, mobile (via `handle_submission`), AR, video, manual. All create billing addresses from parsed fields via `merge_address_parts()`. Manual mode has volume fast-path (skip vision pipeline).

## Test Infrastructure

- `src/test_helpers.rs` — DB pool factory, JWT generator, insert factories (customer, address, inquiry, employee, day, day-employee, estimation)
- `tests/integration_tests.rs` — 20 DB-level integration tests requiring `DATABASE_URL`
- Unit tests in `#[cfg(test)] mod tests` blocks within source files (repos, routes, services)

## When Adding a New Endpoint

1. Add repo function in `repositories/`
2. Add route handler in `routes/`
3. Wire route in `routes/mod.rs`
4. Add integration test in `tests/integration_tests.rs`
5. Update `docs/API.md`