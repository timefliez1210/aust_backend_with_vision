# crates/volume-estimator — Volume Calculation Client

Thin HTTP client that calls the Python vision service for volume estimation.

## Estimation Methods

```rust
pub enum EstimationMethod {
    Vision,       // photo upload (no depth maps)
    Inventory,    // manual item list form
    DepthSensor,  // depth maps present
    Ar,           // AR phone scan (server-side pipeline)
    ArDevice,     // AR phone scan, volume computed on-device (LiDAR + Swift OBB, "ar_device")
    Video,        // MASt3R video reconstruction
    Manual,       // admin/customer-provided volume
}
```

This enum lives in `crates/core/src/models/volume.rs`, not in this crate.

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
- `label_objects()` is a second, naming-only mode: one German name per photo, no
  volume, no dedup. Used for AR items the customer measured on-device but left
  unnamed (the app's capture screen makes naming optional). Batches of 8 photos
  keep photo↔name ordering reliable; every failure path returns `FALLBACK_LABEL`
  ("Möbelstück") rather than an error, so a missing name can't cost a measurement

### 2. ML Vision Service (`VisionServiceClient`)
- HTTP client for `services/vision/` (`vision_service.backend = "modal"`)
- Grounding DINO → SAM2 → Depth Anything V2 → OBB
- GPU-heavy; known ~1.9× over-count from cross-image duplicates
- Still the only path for AR per-item capture

### 3. LLM Vision (`VisionAnalyzer`)
- Sends base64 images one-by-one to the configured LLM with a generic prompt
- Legacy fallback; no RE catalogue, no cross-photo dedup

## Client Methods (`VisionServiceClient`, `src/vision_service.rs`)

```rust
let client = VisionServiceClient::new(base_url, video_base_url, ar_base_url, timeout_secs, max_retries)?;
```

`video_base_url`/`ar_base_url` are `Option<&str>`, each falling back to `base_url` when `None` — lets photo, video, and AR capture point at different Modal deployments. There is no `check_ready()`.

Two calling conventions coexist:
- **Synchronous, multipart upload**: `estimate_upload(job_id, images)` / `estimate_video(job_id, video_bytes, ...)` — block until the pipeline finishes, retrying the HTTP call itself with exponential backoff (`1 << attempt` seconds) up to `max_retries` via the internal `send_with_retry` helper.
- **Async submit/poll**: `submit_upload(...)` / `submit_video(...)` / `submit_ar(...)` return a `VisionSubmitResponse { job_id, status: "accepted" }` immediately; `poll_job_status` / `poll_video_job_status` / `poll_ar_job_status` return `VisionJobStatus { status, result, error }` until `status` is `"succeeded"`/`"failed"`. `estimate_upload_async`/`estimate_video_async`/`estimate_ar_async` wrap the submit+poll loop for callers that just want the final result; on a `failed`/`not_found` poll they resubmit (flat 5s delay, not exponential) up to `max_retries` times.

Response: `VisionServiceResponse { job_id, status, detected_items: Vec<VisionDetectedItem>, total_volume_m3, confidence_score, processing_time_ms }`. `VisionDetectedItem` carries the RE-lookup fields (`german_name`, `re_value`, `units`, `volume_source`, `is_moveable`, `packs_into_boxes`) alongside the geometric ones (`dimensions`, `bbox`, `crop_base64`).

## Error Variants

`VolumeError`: Vision, Inventory, Llm, Storage, ExternalService(String), InvalidData

## Configuration

Uses `VisionServiceConfig` from core: `enabled` (default `false`), `base_url` (default `http://localhost:8090`), `video_base_url`/`ar_base_url` (both `Option<String>`, default `None` → fall back to `base_url`), `timeout_secs` (default 120), `max_retries` (default 1), `poll_interval_secs` (default 60 — Modal containers stay warm at least that long), `max_polls` (default 20, i.e. a 20-minute ceiling for photo jobs; video may need more), plus VLM backend selection: `backend` (`"modal"` default | `"vlm"`), `vlm_model` (default `"minimax-m3"`), `vlm_timeout_secs` (default 1800).

## ⚠️ Connected Changes

| If you change... | ...also verify |
|---|---|
| `EstimationMethod` enum variants | `volume.rs` in core, `submissions.rs` handler dispatch, `offer_builder.rs` `parse_detected_items()` |
| `VisionServiceClient` interface or retry logic | `submissions.rs` photo/mobile handlers, `offer_pipeline.rs` auto-offer trigger, `vision.rs` service wrapper |
| Method string values (e.g. "ar", "depth_sensor") | `submissions.rs` parsing, frontend estimation display — `volume_estimations.method` is a plain `VARCHAR(50)`, **no DB CHECK constraint enforces the value set** |
