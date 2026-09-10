# crates/distance-calculator — OpenRouteService Route Calculation

Single-purpose crate: multi-stop route distance calculation via ORS API.

## Key Function

```rust
pub async fn calculate(&self, req: &RouteRequest) -> Result<RouteResult, RouteError>
```

`RouteRequest { addresses: Vec<String> }` — ordered waypoints. Returns `RouteResult { total_distance_km, total_duration_hours }`.

## Architecture

```
RouteCalculator        (public, high-level orchestrator)
  ├── Geocoder         (address string → lat/lng)
  └── DistanceRouter   (lat/lng pair → km + minutes)
```

| File | Responsibility |
|------|----------------|
| `route.rs` | `RouteCalculator` — chains geocoding + routing for N addresses, computes price |
| `geocoder.rs` | `Geocoder` — calls ORS Geocode Search API, scoped to DE/AT |
| `router.rs` | `DistanceRouter` — calls ORS Directions API for driving distance |
| `error.rs` | `DistanceError` — error enum for all failure modes |

## ORS API Details

| Endpoint | Purpose | Rate Limit (free) |
|----------|---------|-------------------|
| `GET /geocode/search` | Address → coordinates | 40 req/min |
| `GET /v2/directions/driving-car` | Point-to-point driving route | 40 req/min |

**Auth**: API key via `AUST__MAPS__API_KEY`, passed as `?api_key=` query param.

**Critical headers**: Geocoding needs `Accept: application/json`. Directions needs `Accept: application/geo+json;charset=UTF-8` — **will return 406 without this header**.

## Usage in Backend

Called from `offer_builder.rs::build_fahrt_item()` (Fahrkostenpauschale) and `submissions.rs` (ORS distance for manual inquiries). Also from `offer_pipeline.rs` (auto-offer when distance missing).

## Pricing

`RouteCalculator::calculate` also returns a `price_cents` field: `ceil(total_distance_km) × PRICE_PER_KM_CENTS`, with `PRICE_PER_KM_CENTS = 100` (€1.00/km) hardcoded as a constant in `route.rs`. This field is only used by the standalone `GET` distance route (`routes/distance.rs`).

**The KVA's actual Fahrkostenpauschale ignores this field.** `offer_builder.rs::build_fahrt_item()` takes only `RouteResult.total_distance_km` from this crate and multiplies it by the config-driven `CompanyConfig::fahrt_rate_per_km` (default `1.0`, same €1/km, but it can be changed via settings without touching this crate). If you change the pricing customers actually pay, that rate lives in core config / `settings_repo.rs`, not here.

## Testing

Real API tests require `AUST__MAPS__API_KEY`. Unit tests cover parsing and error cases.
## ⚠️ Connected Changes

| If you change... | ...also verify |
|---|---|
| Route calculation or pricing | `offer_builder.rs` Fahrkostenpauschale (depot→origin→stop→dest→depot), `submissions.rs` ORS distance for manual inquiries, `offer_pipeline.rs` auto-offer distance calculation |
| ORS API or response format | `geocoder.rs` address parsing, `router.rs` response deserialization, error handling tests |
