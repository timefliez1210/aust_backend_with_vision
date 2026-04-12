# crates/core — Domain Models, Config, Shared Types

The shared foundation. Every other crate depends on this. No DB, no HTTP, no I/O — pure domain logic.

## Key Files

| File | What | Key Types |
|------|------|-----------|
| `src/config.rs` | Application configuration | `Config`, `CompanyConfig` (pricing rates), `CalendarConfig`, `VisionServiceConfig` |
| `src/models/inquiry.rs` | Inquiry lifecycle | `InquiryStatus` state machine with `can_transition_to()` + `is_locked_for_modifications()` |
| `src/models/offer.rs` | Offer state | `OfferStatus`, `PricingBreakdown`, `PricingInput`, `PricingResult` |
| `src/models/snapshots.rs` | Structured services | `Services` (packing, assembly, disassembly, storage, disposal, parking_ban_origin/destination) |
| `src/models/volume.rs` | Estimation methods | `EstimationMethod` enum (Vision, Inventory, DepthSensor, Ar, Video, Manual) |
| `src/models/user.rs` | Auth | `TokenClaims`, `UserRole` (Admin, Buerokraft, Employee) |
| `src/error.rs` | Shared errors | |

## InquiryStatus State Machine

```
pending → info_requested → estimating → estimated → offer_ready → offer_sent
  → accepted | rejected | expired | cancelled
  → scheduled → completed → invoiced → paid
```

- `can_transition_to(&self, target)` — enforces valid transitions
- `is_locked_for_modifications(&self)` — returns true for `offer_ready` through `paid` (prevents volume/address changes after offer)

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
| `saturday_surcharge_cents` | i64 | 5000 | Saturday surcharge (€50) |

## What NOT to put here

- SQL queries — go in `crates/api/src/repositories/`
- HTTP handlers — go in `crates/api/src/routes/`
- Business logic that touches DB/IO — goes in `crates/api/src/services/`
- Only pure domain models, config structs, and shared types belong here
## ⚠️ Connected Changes

| If you change... | ...also verify |
|---|---|
| `InquiryStatus` enum | `can_transition_to()`, `is_locked_for_modifications()`, admin frontend `INQUIRY_STATUS_LABELS`, inquiry PATCH handler status gate |
| `CompanyConfig` struct | `PricingEngine::with_rate()` calls, `ServicePrices::from_config()`, offer generator, all unit tests using `PricingEngine::new()` |
| `Services` struct | `build_line_items()` in offer builder, XLSX line items, foto-angebot form, admin service toggles |
| `EstimationMethod` enum | `volume.rs` string conversion, 5 submission handlers, DB CHECK constraint, offer builder `parse_detected_items()`, vision service |
