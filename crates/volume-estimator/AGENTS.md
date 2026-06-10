# crates/volume-estimator — Volume Calculation Client

Thin HTTP client that calls the Python vision service for volume estimation.

## Estimation Methods

```rust
pub enum EstimationMethod {
    Vision,       // photo upload (no depth maps)
    Inventory,    // manual item list form
    DepthSensor,  // depth maps present
    Ar,           // AR phone scan
    Video,        // MASt3R video reconstruction
    Manual,       // admin/customer-provided volume
}
```

Not a DB enum — parsed from string. `as_str()` returns lowercase snake_case. `from_str()` is lenient (accepts `depth_sensor` and `depth_maps`).

## Three Vision Approaches

### 1. Catalogue-grounded VLM (`VlmEstimator`) — preferred
- Full apartment photos + RE catalogue in ONE vision-model pass via Ollama Cloud
  (`vision_service.backend = "vlm"`, model from `vision_service.vlm_model`,
  connection from `llm.ollama.base_url`/`api_key`)
- Scene-level cross-photo dedup (the crop pipeline's unfixable weakness);
  59-photo benchmark: minimax-m3 32.6 m³ vs ~37 m³ gold vs 60.5 m³ crop pipeline
- Video: ffmpeg keyframes (≤40, duration-spanning) → same photo path
  (ffmpeg/ffprobe required — present in `docker/Dockerfile.backend`)
- Uses `OllamaProvider::complete_streaming` — thinking models (minimax-m3 ~14 min
  on 59 photos) stall on non-streaming requests
- Catalogue prompt embedded from `src/re_catalogue.txt`; regenerate via
  `services/vision/vlm_cloud_eval.py::build_catalogue()` when `RE_CATALOG` changes
- Totals recomputed server-side from line volumes; the model's own total is ignored

### 2. ML Vision Service (`VisionServiceClient`)
- HTTP client for `services/vision/` (`vision_service.backend = "modal"`)
- Grounding DINO → SAM2 → Depth Anything V2 → OBB
- GPU-heavy; known ~1.9× over-count from cross-image duplicates
- Still the only path for AR per-item capture

### 3. LLM Vision (`VisionAnalyzer`)
- Sends base64 images one-by-one to the configured LLM with a generic prompt
- Legacy fallback; no RE catalogue, no cross-photo dedup

## Client Methods

```rust
let client = VisionServiceClient::new(base_url, timeout_secs, max_retries, retry_delay_secs);
let ready = client.check_ready().await?;
let result = client.estimate_images(req).await?;
```

- Retries with exponential backoff (2^n seconds)
- Request: `VisionServiceRequest { job_id, s3_keys, options }`
- Response: `VisionServiceResponse { job_id, status, detected_items, total_volume_m3, ... }`

## Error Variants

`VolumeError`: Vision, Inventory, Llm, Storage, ExternalService(String), InvalidData

## Configuration

Uses `VisionServiceConfig` from core: `enabled`, `base_url`, `timeout_secs` (default 120), `max_retries` (default 1), plus VLM backend selection: `backend` (`"modal"` default | `"vlm"`), `vlm_model` (default `"minimax-m3"`), `vlm_timeout_secs` (default 1800).
## ⚠️ Connected Changes

| If you change... | ...also verify |
|---|---|
| `EstimationMethod` enum variants | `volume.rs` in core, `submissions.rs` handler dispatch, DB CHECK constraint, `offer_builder.rs` `parse_detected_items()` |
| `VisionServiceClient` interface or retry logic | `submissions.rs` photo/mobile handlers, `offer_pipeline.rs` auto-offer trigger, `vision.rs` service wrapper |
| Method string values (e.g. "ar", "depth_sensor") | DB `volume_estimations.method` CHECK constraint, `submissions.rs` parsing, frontend estimation display |
