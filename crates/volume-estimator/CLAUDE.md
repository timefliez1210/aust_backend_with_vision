# crates/volume-estimator — Vision Service Client

> **Full context**: [AGENTS.md](AGENTS.md)

HTTP client for the Python vision service. `EstimationMethod` (defined in core): Vision,
Inventory, DepthSensor, Ar, ArDevice, Video, Manual — `ArDevice` is on-device LiDAR from
the mobile app and never calls this client.

Three backends: catalogue-grounded VLM (preferred), the Modal ML service, and a legacy
per-image LLM fallback. Retries with exponential backoff.

See [AGENTS.md](AGENTS.md) for: method mapping, client methods, error variants, config.