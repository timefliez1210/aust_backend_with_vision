# crates/core — Domain Models & Configuration

> Key types and how they flow through the system: [../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md#key-data-types-flowing-through-the-pipeline)

Shared foundation crate used by all other crates. Contains domain models, application configuration, and common error types.

## Key Files

- `src/config.rs` - Master `Config` struct with all nested sections
- `src/models/volume.rs` - Volume estimation domain types
- `src/models/email.rs` - Email/inquiry domain types
- `src/error.rs` - Shared error types
- `src/lib.rs` - Re-exports

## Configuration Structs

`Config` is the root, deserialized from `config/*.toml` with env var overrides (`AUST__SECTION__KEY`):

| Struct | Fields | Purpose |
|--------|--------|---------|
| `ServerConfig` | host, port | HTTP server binding |
| `DatabaseConfig` | url, max_connections | PostgreSQL |
| `RedisConfig` | url | Redis cache |
| `StorageConfig` | provider, bucket, endpoint, region, access_key, secret_key | S3/MinIO |
| `EmailConfig` | imap_*, smtp_*, poll_interval_secs, from_address | Email I/O |
| `LlmConfig` | default_provider, claude/openai/ollama sub-configs | LLM selection |
| `MapsConfig` | provider, api_key | OpenRouteService |
| `TelegramConfig` | bot_token, admin_chat_id | Telegram bot |
| `AuthConfig` | jwt_secret, jwt_expiry_hours | JWT auth |
| `CalendarConfig` | default_capacity, alternatives_count, search_window_days | Booking |
| `VisionServiceConfig` | enabled, base_url, timeout_secs, max_retries | ML vision service |

## Domain Models

### Volume Estimation (`models/volume.rs`)

- `EstimationMethod` — enum: Vision, Inventory, DepthSensor, Manual
- `VolumeEstimation` — full DB record (id, quote_id, method, total_volume_m3, confidence_score, etc.)
- `VisionAnalysisResult` / `DetectedItem` — LLM vision results
- `DepthSensorResult` / `DepthSensorItem` — 3D pipeline results
- `ItemDimensions` — (length_m, width_m, height_m)
- `InventoryItem` / `InventoryForm` — manual inventory input

### Email (`models/email.rs`)

- `MovingInquiry` — aggregated customer inquiry state (name, phone, addresses, dates, volume, services, notes)
- `ParsedEmail` — structured email parsing output
- `MissingField` — tracks data gaps in an inquiry

## Dependencies

serde, chrono, uuid — no external API calls, pure data types.

## Usage

Every other crate depends on `aust-core` for shared types and config.
