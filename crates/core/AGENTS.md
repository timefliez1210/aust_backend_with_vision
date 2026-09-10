# crates/core — Domain Models, Config, Shared Types

The shared foundation. Every other crate depends on this. No DB, no HTTP.
(`events` does hold `sqlx`-backed reader/writer helpers for the `domain_events`
table — the one exception; see below.)

## Key Files

| File | What | Key Types |
|------|------|-----------|
| `src/config.rs` | Application configuration | `Config`, `CompanyConfig` (pricing rates), `CalendarConfig`, `VisionServiceConfig` |
| `src/models/inquiry.rs` | Inquiry lifecycle | `InquiryStatus` state machine with `can_transition_to()`, `Inquiry`, `MovingInquiry`, `MissingField` |
| `src/models/offer.rs` | Offer state | `OfferStatus`, `Offer`, `PricingBreakdown`, `PricingInput`, `PricingResult` |
| `src/models/snapshots.rs` | Structured services + canonical response | `Services`, `InquiryResponse` (built by `crates/api/src/services/inquiry_builder.rs`) |
| `src/models/volume.rs` | Estimation methods | `EstimationMethod` enum |
| `src/models/user.rs` | Auth | `TokenClaims`, `UserRole` |
| `src/models/{address,customer,employee,note,email}.rs` | Row/DTO structs (`Address`, `Customer`, `Employee`, `Note`, `EmailThread`/`EmailMessage`, etc.) | plain data, no logic |
| `src/services/` | Trait abstractions consumed by `crates/assistant` | see below |
| `src/events/` | Domain-event emit/consume helpers (DB-backed) | `EventEmitter`, `EventConsumer` |
| `src/error.rs` | Shared errors | |

## InquiryStatus State Machine

```
pending → info_requested → estimating → estimated → offer_ready → offer_sent
  → accepted | rejected | expired | cancelled
  → scheduled → completed → invoiced → paid
```

- `can_transition_to(&self, target)` — currently returns `true` for all transitions
  (admin dashboard has full flexibility to correct mistakes; the state machine is
  informational only, not enforced).
- `to_offer_status()` maps the offer-relevant subset (`OfferReady`→"draft",
  `OfferSent`→"sent", `Accepted`, `Rejected`, `Expired`, `Cancelled`) back to the
  legacy `OfferStatus` string; everything else is `None`.

## `Services` (structured service flags, stored as JSONB on `inquiries.services`)

`packing`, `assembly`, `disassembly`, `storage`, `disposal`,
`parking_ban_origin`, `parking_ban_destination`, `transporter` — all `bool`,
default `false`.

## `EstimationMethod`

`Vision`, `Inventory`, `DepthSensor`, `Ar`, `ArDevice`, `Video`, `Manual`.
`ArDevice` (on-device LiDAR + OBB volume from the mobile app) is a separate
variant from `Ar` (server-side per-item 3D reconstruction) — easy to miss when
grepping for "Ar".

## `UserRole`

`Admin`, `Buerokraft` (default; office manager — can't delete customers/employees),
`Operator` (legacy alias kept only for backwards compatibility with existing tokens
— new code should use `Buerokraft`).

## `src/services/` — trait abstractions for the assistant

Decouples `crates/assistant` from `crates/api` to avoid a circular dependency
(assistant → core ← api instead of assistant ↔ api):

- `traits.rs` — one trait per business domain (`InquiryService`, `OfferService`,
  `CalendarService`, `CustomerService`, `EmailService`, `InvoiceService`,
  `EmployeeService`, `EstimationService`, `AddressService`, `SettingsService`,
  `ReviewService`, `MetricsService`, `TodoService`, `ReminderService`) plus their
  shared DTOs (`OfferDraft`, `OfferComputation`, `ComputedLineItem`, etc.).
- `bundle.rs` — `ServiceBundle`, a cloneable struct of `Arc<dyn ...Service>` for
  every trait above. Built once at API startup and injected into the assistant's
  `ToolCtx`.
- `error.rs` — `ServiceError`.

Concrete implementations (`*ServiceImpl`) live in `crates/api/src/services/bridge/`.
Add a method to the trait here first, then implement it in the bridge, when a new
assistant tool needs new backend functionality.

## `src/events/` — domain events

`EventEmitter::emit(kind, aggregate, payload)` inserts a row into `domain_events`
(e.g. `kind: "inquiry.created"`, `aggregate: "inquiry:<uuid>"`). Emission is
**non-fatal** — callers log a warning and continue on failure; the DB transaction
that made the fact true is the system of record, not the event log.
`EventConsumer::new(pool, consumer_name)` reads events not yet consumed by that
name (`fetch_pending`) and marks them consumed (`mark_consumed`, JSONB merge so
multiple named consumers don't clobber each other's marks). Written by the API
layer, consumed by the assistant's event loop.

## CompanyConfig Pricing Constants

All configurable via `config/*.toml` with `serde(default)`:

| Field | Type | Default | Purpose |
|-------|------|---------|---------|
| `depot_address` | String | "Borsigstr 6 31135 Hildesheim" | ORS route start/end |
| `fahrt_rate_per_km` | f64 | 1.0 | Per-km travel charge (€) |
| `rate_per_person_hour_cents` | i64 | 3000 | Labor rate (€30/hr) |
| `assembly_price` | f64 | 25.0 | De/Montage per unit (€) |
| `parking_ban_price` | f64 | 100.0 | Halteverbotszone per zone (€) |
| `packing_price` | f64 | 30.0 | Umzugsmaterial (€) |
| `transporter_price` | f64 | 60.0 | 3,5t Transporter m. Koffer (€) |
| `saturday_surcharge_cents` | i64 | 5000 | Saturday surcharge (€50) |

## What NOT to put here

- SQL queries — go in `crates/api/src/repositories/` (the `events` module is the
  one sanctioned exception, since both API and assistant need the same reader/writer)
- HTTP handlers — go in `crates/api/src/routes/`
- Business logic that touches DB/IO beyond the trait interface above — goes in
  `crates/api/src/services/`
- Only pure domain models, config structs, service trait interfaces, and shared
  types belong here

## ⚠️ Connected Changes

| If you change... | ...also verify |
|---|---|
| `InquiryStatus` enum | `can_transition_to()`, admin frontend `INQUIRY_STATUS_LABELS`, `inquiries.rs` PATCH handler status validation |
| `CompanyConfig` struct | `PricingEngine::with_rate()` calls, `ServicePrices::from_pricing()` (in `crates/api/src/services/offer_builder.rs`), offer generator, all unit tests using `PricingEngine::new()` |
| `Services` struct | `build_line_items()` in offer builder, XLSX line items, foto-angebot form, admin service toggles |
| `EstimationMethod` enum | `volume.rs` string conversion, the 5 submission handlers, `parse_detected_items()` in the offer builder, the vision service — `volume_estimations.method` is a plain VARCHAR with no CHECK constraint, so nothing rejects a typo |
| `services/traits.rs` trait signatures | matching impl in `crates/api/src/services/bridge/`, any `crates/assistant` tool calling that method |
