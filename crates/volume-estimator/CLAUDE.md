# crates/volume-estimator — Vision Service Client

> **Full context**: [AGENTS.md](AGENTS.md)

HTTP client for the Python vision service. `EstimationMethod` enum: Vision, Inventory, DepthSensor, Ar, Video, Manual.

ML service first, LLM fallback if unavailable. Retries with exponential backoff.

See [AGENTS.md](AGENTS.md) for: method mapping, client methods, error variants, config.