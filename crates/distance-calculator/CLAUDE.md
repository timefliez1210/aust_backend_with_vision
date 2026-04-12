# crates/distance-calculator — ORS Route Calculation

> **Full context**: [AGENTS.md](AGENTS.md)

Multi-stop route distance via OpenRouteService. `RouteCalculator::calculate()` takes ordered waypoints, returns km + duration.

**Critical**: ORS Directions API returns 406 without `Accept: application/geo+json;charset=UTF-8` header.

See [AGENTS.md](AGENTS.md) for: file map, ORS API details, pricing, error handling.