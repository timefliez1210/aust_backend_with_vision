# services/vision — Python ML Pipeline (GPU)

FastAPI service for 3D volume estimation from photos and video. No local GPU —
the service only runs for real on Modal (serverless L4). See `docs/VISION.md`
for the full pipeline explanation; this file is the file map + wiring.

## Local FastAPI app (`app/main.py`)

Synchronous routes, used for local dev/testing against a GPU box or CPU stub —
**not** what production calls:

```
POST /estimate/images   → photo pipeline, sync (app/api/endpoints/estimate.py)
POST /estimate/video    → video pipeline, sync (app/api/endpoints/video.py)
GET  /health            → liveness, always 200
GET  /ready             → readiness, 503 until model_loader.registry.is_loaded
```

`/estimate/images` takes `{job_id, s3_keys, options}`, downloads from S3, and
runs `VisionPipeline.run()` inline before responding. `/estimate/video` takes
a multipart video file (`≤500MB`, `video/*`) and runs `VideoPipeline.run()`
inline (2-10 min — this blocks the request; there is no async job store here).

The AR per-item pipeline (`app/vision/ar_pipeline.py`) exists as code but is
**not wired into this local router** — it is only reachable through Modal's
`serve_ar` (see below).

## Production: Modal (`modal_app.py`)

Production does not call the local FastAPI app. It calls three independent
Modal ASGI apps, each pairing a no-GPU HTTP layer with a GPU worker class,
using an async submit/poll pattern (job results live in a shared `modal.Dict`,
so a poll survives container restarts):

| HTTP layer (`@app.function`) | GPU worker (`@app.cls`, `gpu="L4"`) | Endpoints |
|---|---|---|
| `serve` | `PhotoPipeline` | `POST /estimate/submit`, `GET /estimate/status/{job_id}`, `POST /estimate/upload` (deprecated sync alias) |
| `serve_video` | `VideoPipeline` | `POST /estimate/video/submit`, `GET /estimate/video/status/{job_id}`, `POST /estimate/video` (deprecated sync alias) |
| `serve_ar` | `ARPipeline` | `POST /estimate/ar/submit`, `GET /estimate/ar/status/{job_id}` |

HTTP layers: `scaledown_window=60`, `max_containers=2`, `timeout=60`, no GPU.
GPU workers: `scaledown_window=120`, `max_containers=1`; `PhotoPipeline` and
`serve_ar`'s worker `timeout=1800`, `VideoPipeline` `timeout=900`. Models load
once per container via `@modal.enter()` and are reused across jobs.

The Rust backend's `VisionServiceClient` (`crates/volume-estimator/src/vision_service.rs`)
talks to the async submit/status endpoints above (`base_url`, `video_base_url`,
`ar_base_url` are three separate Modal app URLs) — it does not call `/estimate/images`
or the local `/estimate/video`.

## Key Files

| File | Purpose |
|------|---------|
| `app/main.py` | Local FastAPI app, route wiring (`app/api/router.py`) |
| `app/api/endpoints/estimate.py` | `POST /estimate/images` (sync photo) |
| `app/api/endpoints/video.py` | `POST /estimate/video` (sync video) |
| `app/api/endpoints/health.py` | `/health`, `/ready` |
| `app/vision/pipeline.py` | `VisionPipeline` — photo orchestration (detect → segment → depth → volume → dedup) |
| `app/vision/video_pipeline.py` | `VideoPipeline` — keyframe → MASt3R → SAM2 → OBB orchestration |
| `app/vision/ar_pipeline.py` | `ARVisionPipeline` — AR per-item pipeline, Modal-only |
| `app/models/schemas.py` | Pydantic request/response models; `RE_CATALOG` (74-item German Umzugsgutliste) |
| `app/vision/detector.py` | Grounding DINO detection, German prompts |
| `app/vision/segmenter.py` | SAM 2 segmentation |
| `app/vision/video_segmenter.py` | SAM 2 video predictor (temporal mask propagation) |
| `app/vision/depth.py` | Depth Anything V2 monocular depth |
| `app/vision/reconstructor.py` | MASt3R multi-view stereo reconstruction |
| `app/vision/keyframe.py` | Video keyframe extraction (scene-change + blur rejection) |
| `app/vision/clip_dedup.py` | CLIP-based cross-image dedup (pre-filter before VLM dedup) |
| `app/vision/vlm_dedup.py` | Qwen2-VL-7B cross-image dedup + label correction |
| `app/vision/volume.py` | `VolumeCalculator` — OBB volume from point clouds |
| `app/vision/model_loader.py` | `ModelRegistry` — lazy load/unload, GPU memory swapping |
| `app/config.py` | `Settings` (env-prefixed `VISION_`) — thresholds, keyframe counts, MASt3R params |
| `modal_app.py` | Production Modal deployment: `serve`/`serve_video`/`serve_ar` + their GPU worker classes |
| `vlm_ollama_eval.py`, `vlm_cloud_eval.py` | Standalone architecture experiments (full-image VLM vs. crop pipeline) — **not** the production pipeline, separate Modal app `aust-vlm-eval` |

## Estimation Method Mapping

| Method | `estimation_method` string | Pipeline |
|--------|--------------------------|----------|
| Photo upload | `vision` | Grounding DINO → SAM2 → Depth Anything → RE lookup / OBB |
| AR per-item | `ar` | DINO prompt → SAM2 → MASt3R → OBB (Modal `serve_ar` only) |
| Video | `video` | MASt3R + SAM2 multi-view → RE lookup / OBB |
| VLM (Rust-side) | — | `AUST__VISION_SERVICE__BACKEND=vlm` bypasses this Python service entirely; see `crates/volume-estimator/src/vlm.rs` |
| On-device (LiDAR) | `ar_device` | iOS app computes volume on-device; backend short-circuits, no ML service call |
| Manual | `manual` | No ML, customer-provided volume |
| Inventory form | `inventory` | Parsed from VolumeCalculator items list |

## Deployment

**Local**: `cd services/vision && uvicorn app.main:app --port 8090` (see `app/config.py` for `port` default).
**Production**: `modal deploy modal_app.py` → deploys `serve`, `serve_video`, `serve_ar` as separate Modal apps/URLs.

## ⚠️ Connected Changes

| If you change... | ...also verify |
|---|---|
| New estimation method or `EstimationMethod` | `volume.rs` enum variants + `from_str()`/`as_str()`, `submissions.rs` handler dispatch, DB CHECK constraint (needs migration), `offer_builder.rs` `parse_detected_items()` |
| Response format or `DetectedItem` schema (`app/models/schemas.py`) | `offer_builder.rs` item sheet generation, `volume.rs` deserialization structs, frontend estimation items table |
| Model weights or inference pipeline | GPU memory management in `model_loader.py` (L4 24GB budget), Modal deployment config, processing time estimates |
| `RE_CATALOG` entries | `crates/volume-estimator/src/re_catalogue.txt` (kept in sync manually — no shared source of truth across Python/Rust) |
