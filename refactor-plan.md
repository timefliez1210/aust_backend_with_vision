# Refactor Plan

## Context & Motivation

The current codebase is maintainable as-is, but adding billing, employee management, and accounting will break it without a structural foundation. This plan establishes that foundation before those features land.

**Target end-state**: A full CRM — moving job lifecycle from first inquiry through billing, employee assignment, expense tracking, and monthly accountant summaries.

---

## Domain Boundaries

The core insight driving this refactor: the current `Quote → Offer` chain is not two entities, it is one entity (`Inquiry`) with a lifecycle. Everything attached to that chain (volume estimation, pricing, PDF) is derived state, not independent resources.

Two phases of the business map to two distinct domain boundaries:

```
PRE-SALES (now)                  OPERATIONS (soon)
────────────────────────         ──────────────────────────────────
Inquiry (received → accepted)    Job (scheduled → completed → paid)
  ├── Estimation                    ├── Employee assignments
  ├── Pricing / Offer               ├── Expenses
  └── Email thread                  ├── Invoice → Payment
                                    └── Monthly summary → Accountant
```

An accepted `Inquiry` creates a `Job`. The seam between them is the single status transition: `inquiry.status = accepted → INSERT INTO jobs`.

---

## Phase 1 — Collapse Pre-Sales into Inquiries + Customers

### 1.1 API Surface

Reduce the current endpoint sprawl to two primary resources:

**Current → Target**

| Current | Target | Notes |
|---|---|---|
| `POST/GET/PATCH /api/v1/quotes` | `POST/GET/PATCH /api/v1/inquiries` | rename + merge |
| `POST /api/v1/offers/generate` | `PATCH /api/v1/inquiries/{id}` with action | state transition |
| `GET /api/v1/offers/{id}` | embedded in `GET /api/v1/inquiries/{id}` | denormalized response |
| `GET /api/v1/offers/{id}/pdf` | `GET /api/v1/inquiries/{id}/pdf` | |
| `POST /api/v1/estimates/{method}` | triggered internally | not exposed as routes |
| `POST /api/v1/distance/calculate` | triggered internally on inquiry create/update | not exposed |
| `GET/PATCH /api/v1/customers` | unchanged | already clean |
| `GET /api/v1/inquiries/{id}/emails` | sub-resource, keep | |
| `/api/v1/calendar/*` | unchanged | genuinely separate domain |
| `/api/v1/auth/*` | unchanged | |

**Target API surface:**
```
POST   /api/v1/customers
GET    /api/v1/customers
GET    /api/v1/customers/{id}
PATCH  /api/v1/customers/{id}

POST   /api/v1/inquiries
GET    /api/v1/inquiries              ?status=&customer_id=&from=&to=
GET    /api/v1/inquiries/{id}
PATCH  /api/v1/inquiries/{id}
GET    /api/v1/inquiries/{id}/pdf
GET    /api/v1/inquiries/{id}/emails

+ health, auth, calendar (unchanged)
```

### 1.2 Canonical Response Type

All the duplicated DTOs (`QuoteCustomer`, `CustomerInfo`, `CustomerRow`, `QuoteAddress`, `AddressInfo`, `EstimationInfo` × 2, `QuoteInfo`, `EnrichedQuote`, `QuoteSummary`, `QuoteDetail`) collapse into one struct:

```rust
// crates/core/src/models/inquiry.rs
pub struct InquiryResponse {
    pub id: Uuid,
    pub status: InquiryStatus,
    pub customer: CustomerSnapshot,
    pub origin: AddressSnapshot,
    pub destination: AddressSnapshot,
    pub stop: Option<AddressSnapshot>,
    pub estimation: Option<EstimationSnapshot>,
    pub pricing: Option<PricingSnapshot>,
    pub offer: Option<OfferSnapshot>,
    pub preferred_date: Option<NaiveDate>,
    pub notes: Option<String>,
    pub services: Services,               // packing, assembly, etc.
    pub inquiry_received_at: DateTime<Utc>,
    pub offer_sent_at: Option<DateTime<Utc>>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

`CustomerSnapshot`, `AddressSnapshot`, etc. are small flat structs defined once in `crates/core`, imported everywhere. No more per-route inline structs.

### 1.3 Status State Machine

```
pending
  → estimating       (images/form received, vision pipeline running)
  → priced           (estimation done, pricing calculated)
  → offer_draft      (offer generated, awaiting Telegram approval)
  → offer_sent       (approved + emailed to customer)
  → accepted         (customer confirmed)
  → rejected         (customer declined)
  → expired          (offer validity lapsed)
  → cancelled
  → done             (job completed — links to Job entity)
  → paid
```

### 1.4 Lifecycle Timestamp Fields

Fields to add to the `quotes` (→ `inquiries`) table:

| Field | Type | Set when | Notes |
|---|---|---|---|
| `inquiry_received_at` | `TIMESTAMPTZ` | inquiry created | actual email Date header / form submit time, not DB insert time |
| `offer_sent_at` | `TIMESTAMPTZ` | Telegram approves + email sent | denormalized from `offers.sent_at` for efficient list queries |
| `accepted_at` | `TIMESTAMPTZ` | customer confirms | currently lost — status flips but timestamp is overwritten by `updated_at` |

Field to add to `offers`:

| Field | Type | Set when | Notes |
|---|---|---|---|
| `payment_due_date` | `DATE` | offer sent | **OPEN DECISION**: default net+14 days from `sent_at`, settable per inquiry. Moves to `invoices` table in Phase 2. |

### 1.5 Database Migration

- Rename `quotes` table to `inquiries` (or keep `quotes` with a view alias during transition)
- Add lifecycle timestamp columns (see 1.4)
- No structural changes to `volume_estimations`, `offers`, `addresses` — they stay as implementation detail tables, not exposed as first-class API resources
- Backfill: set `inquiry_received_at = created_at` for existing rows

### 1.6 Struct Cleanup (crates/api)

Remove all inline route DTOs. Each route file should only contain:
- Request body structs (input validation)
- Handler functions

All response types come from `crates/core::models`. The `crates/api/src/routes/shared.rs` `QuoteRow` DB projection is the one exception — DB row types (FromRow) can stay local to the DB layer.

---

## Phase 2 — Type Safety via ts-rs

Add TypeScript type generation from Rust structs so frontend types can never drift from backend types.

### 2.1 Backend changes

Add `ts-rs` crate. On all canonical response types in `crates/core`:

```rust
#[cfg_attr(feature = "ts", derive(TS), ts(export))]
pub struct InquiryResponse { ... }
```

Add a `ts` feature flag to `crates/core/Cargo.toml`. Add a build step (or `cargo test --features ts`) that exports to `bindings/`.

### 2.2 Frontend changes

- Add a script that copies generated `.ts` files from `aust_backend/bindings/` into `alex_aust/src/lib/types/generated/`
- Remove all manually written types that duplicate backend structs
- `api.svelte.ts` imports from generated types

### 2.3 Scope of generated types

Types to generate (minimum viable set):
- `InquiryResponse`, `InquiryStatus`
- `CustomerSnapshot`, `AddressSnapshot`
- `EstimationSnapshot`, `PricingSnapshot`, `OfferSnapshot`
- `CreateInquiryRequest`, `UpdateInquiryRequest`
- `CustomerResponse`, `CreateCustomerRequest`

---

## Phase 3 — Frontend Refactor (alex_aust)

### 3.1 Admin dashboard simplification

Current admin has separate pages for quotes, offers, estimates. After Phase 1 these all collapse into:
- `/admin/inquiries` — list with status filter
- `/admin/inquiries/[id]` — full detail (estimation, pricing, offer, emails, map all on one page)
- `/admin/customers` — unchanged

### 3.2 API client reorganization

Split `api.svelte.ts` into domain modules:
- `lib/api/inquiries.ts`
- `lib/api/customers.ts`
- `lib/api/calendar.ts`
- `lib/api/auth.ts`
- `lib/utils/format.ts` (currency, date formatting — separate from API calls)

### 3.3 Build pipeline cleanup

Replace `inline-css.py` post-build script with a Vite plugin. Same effect, no external Python dependency, runs as part of `npm run build`.

---

## Phase 4 — Operations Domain (Jobs, Billing, Employees)

> This phase is not planned in detail yet — scoped here for architectural awareness only.

New primary entities added when billing/employee features land:

```
jobs                  created when inquiry.status → accepted
  ├── job_assignments  employee ↔ job (date, hours_worked, role, pay_rate)
  ├── expenses         per-job costs (fuel, packing materials, etc.)
  └── invoices         billing document
        └── payments   money received

employees             reusable staff pool
general_expenses      non-job overhead (rent, tools, insurance)
```

Monthly accountant summary = aggregate query over `jobs`, `invoices`, `payments`, `general_expenses` grouped by month. Clean to implement when the data model above is in place.

---

## Migration Sequence

```
1. Write migration: add lifecycle timestamp columns to quotes/offers
2. Rename/alias quotes → inquiries in DB and crate naming
3. Create InquiryResponse canonical struct in crates/core
4. Rewrite route handlers to use canonical struct (delete inline DTOs)
5. Collapse /quotes, /offers, /estimates/* routes into /inquiries
6. Add ts-rs feature + export step
7. Update frontend: import generated types, split api client, update routes
8. Replace inline-css.py with Vite plugin
9. (Later) Add jobs/employees/billing entities
```

---

## Open Decisions

- **`payment_due_date`**: net+14 default from `offer_sent_at`, settable per inquiry. When billing lands this moves to `invoices`. Decide: store on `offers` table now or on `inquiries`?
- **DB rename**: rename `quotes` → `inquiries` in one migration with `ALTER TABLE`, or keep `quotes` and add an `inquiries` view during transition to avoid a flag day?
- **Offer versions**: currently a unique constraint prevents multiple active offers per quote. In the refactored model, does an inquiry carry one offer or a history of offer versions (after admin edits)? Currently edits regenerate and replace. Keep that behaviour?
