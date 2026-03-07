# Distance Calculator Crate

> External service map: [../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md#external-service-dependencies)
> ORS 406 error fix: [../../docs/DEBUGGING.md#5-ors-directions-api-returns-406](../../docs/DEBUGGING.md)

Multi-stop driving distance and travel time calculator using OpenRouteService (free, OpenStreetMap-based).

## Architecture

```
RouteCalculator        (public, high-level orchestrator)
  ├── Geocoder         (address string → lat/lng)
  └── DistanceRouter   (lat/lng pair → km + minutes)
```

### Files

| File | Responsibility |
|------|----------------|
| `route.rs` | `RouteCalculator` — chains geocoding + routing for N addresses, computes price |
| `geocoder.rs` | `Geocoder` — calls ORS Geocode Search API, scoped to DE/AT |
| `router.rs` | `DistanceRouter` — calls ORS Directions API for driving distance between two points |
| `error.rs` | `DistanceError` — error enum for all failure modes |

## API

### `RouteCalculator::calculate(request) -> RouteResult`

The main entry point. Takes an ordered list of addresses, returns the full route with per-leg and total metrics.

**Input — `RouteRequest`:**
```json
{
  "addresses": [
    "Kaiserstr. 32, 31134 Hildesheim",
    "Marktplatz 1, 38100 Braunschweig",
    "Kröpcke 1, 30159 Hannover"
  ]
}
```

- Minimum 2 addresses, no upper limit
- Free-text German addresses (street, PLZ, city)
- Order matters: route is calculated sequentially 1→2→3→...→N

**Output — `RouteResult`:**
```json
{
  "addresses": [
    "Kaiserstr. 32, 31134 Hildesheim",
    "Marktplatz 1, 38100 Braunschweig",
    "Kröpcke 1, 30159 Hannover"
  ],
  "legs": [
    {
      "from_address": "Kaiserstr. 32, 31134 Hildesheim",
      "to_address": "Marktplatz 1, 38100 Braunschweig",
      "from_location": { "latitude": 52.155802, "longitude": 9.951709 },
      "to_location": { "latitude": 52.272012, "longitude": 10.625469 },
      "distance_km": 62.7,
      "duration_minutes": 46
    },
    {
      "from_address": "Marktplatz 1, 38100 Braunschweig",
      "to_address": "Kröpcke 1, 30159 Hannover",
      "from_location": { "latitude": 52.272012, "longitude": 10.625469 },
      "to_location": { "latitude": 52.374551, "longitude": 9.738476 },
      "distance_km": 73.2,
      "duration_minutes": 57
    }
  ],
  "total_distance_km": 135.9,
  "total_duration_minutes": 103,
  "price_cents": 13600,
  "price_per_km_cents": 100
}
```

## Processing Flow

```
Input: ["Address A", "Address B", "Address C"]
                    │
                    ▼
        ┌───────────────────────┐
        │  Geocode all addresses │  (ORS Geocode Search API)
        │  A → (lat, lng)        │  boundary.country=DE,AT
        │  B → (lat, lng)        │  size=1 (best match)
        │  C → (lat, lng)        │
        └───────────┬───────────┘
                    ▼
        ┌───────────────────────┐
        │  Calculate each leg    │  (ORS Directions API)
        │  A→B: 62.7 km, 46 min │  driving-car profile
        │  B→C: 73.2 km, 57 min │
        └───────────┬───────────┘
                    ▼
        ┌───────────────────────┐
        │  Sum totals + price    │
        │  135.9 km, 103 min     │  price = ceil(km) × €1.00
        │  €136.00               │
        └───────────────────────┘
```

## Pricing

- **Rate**: €1.00 per km (`PRICE_PER_KM_CENTS = 100`)
- **Rounding**: Total km is ceiled before multiplying
- Configured as a constant in `route.rs` — change there to adjust

## External API

**Provider**: [OpenRouteService](https://openrouteservice.org/) (free tier)

| Endpoint | Purpose | Rate Limit (free) |
|----------|---------|-------------------|
| `GET /geocode/search` | Address → coordinates | 40 req/min |
| `GET /v2/directions/driving-car` | Point-to-point driving route | 40 req/min |

**Authentication**: API key via `AUST__MAPS__API_KEY` env var, passed as `?api_key=` query param.

**Important headers**:
- Geocoding: `Accept: application/json`
- Directions: `Accept: application/geo+json;charset=UTF-8` (will 406 without this)

## REST Endpoint

```
POST /api/v1/distance/calculate
Content-Type: application/json

{
  "addresses": ["Addr 1", "Addr 2", ...]
}
```

Returns `RouteResult` JSON. Requires minimum 2 addresses or returns 422.

## Error Handling

| Error | Cause |
|-------|-------|
| `DistanceError::Geocoding` | Address not found or ambiguous |
| `DistanceError::Routing` | Cannot compute driving route (e.g. addresses on different continents) |
| `DistanceError::Api` | ORS returned non-200 or unparseable response |
| `DistanceError::Network` | Connection/TLS failure to ORS |

## Usage in Moving Pipeline

The distance calculator is called after both departure and arrival addresses are known:

```rust
let calculator = RouteCalculator::new(api_key);

// Simple A→B move
let result = calculator.calculate(&RouteRequest {
    addresses: vec![departure, arrival],
}).await?;

// Move with Zwischenstopp
let result = calculator.calculate(&RouteRequest {
    addresses: vec![departure, intermediate, arrival],
}).await?;

// result.total_distance_km → used for offer pricing
// result.total_duration_minutes → shown to customer
// result.price_cents → distance component of final price
```

## Testing

```bash
# Run with real API (needs AUST__MAPS__API_KEY set)
cargo run -p aust-distance-calculator --example test_route
```
