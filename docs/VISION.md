# Vision Pipeline — Technical Reference

How the system estimates moving volume from photos, video, and on-device
capture. See `services/vision/AGENTS.md` for the file-by-file map of the
Python service and the exact Modal wiring.

**Which backend actually runs is chosen per-submission, in priority order**
(`crates/api/src/routes/submissions.rs::process_ar_submission_background`,
`crates/core/src/config.rs::VisionServiceConfig`):

1. **On-device (LiDAR)** — if every item in the client's manifest carries a
   plausible `device_volume_m3`, the app already computed volumes on-device
   and the backend short-circuits: no server-side vision call at all.
   Recorded as `estimation_method = "ar_device"`.
2. **VLM** (`vision_service.backend = "vlm"`) — one representative frame per
   item goes through a catalogue-grounded vision model via Ollama
   (`VlmEstimator` in `crates/volume-estimator/src/vlm.rs`). No Modal call.
   Recorded as `"ar"` (AR submissions) or the equivalent photo/video method.
3. **Modal** (`vision_service.backend = "modal"`, the default enum value) —
   the legacy Python GPU pipeline described below, via async submit + poll.

The code default (`default_vision_backend()` in `crates/core/src/config.rs`)
is `"modal"`, but `.env.example` sets `AUST__VISION_SERVICE__BACKEND=vlm` —
confirm the actual value of that env var on the deployment you're targeting
before assuming which pipeline is live. The Modal pipeline below remains
reachable and is the fallback when `backend != "vlm"`.

---

## Part A — Modal GPU Pipeline (`services/vision/`)

**There is no local GPU.** This pipeline is only meaningful running against
the deployed Modal apps. `modal deploy services/vision/modal_app.py` deploys
three independent apps (`serve`, `serve_video`, `serve_ar`), each a no-GPU
HTTP layer plus a GPU worker class (`gpu="L4"`) — see `services/vision/AGENTS.md`
for the exact endpoints, timeouts, and container settings.

**⚠️ The live Modal deployment predates the CLIP/Qwen cross-image dedup and
the moveable-item filter that exist in `services/vision/app/vision/` today
(`clip_dedup.py`, `vlm_dedup.py`, `is_moveable` on `DetectedItem`). Redeploy
before trusting any evaluation run against it.**

### 1. Photo pipeline

**Entry point (production)**: Modal `serve` app, `POST /estimate/submit` +
`GET /estimate/status/{job_id}` (async). A synchronous `POST /estimate/images`
also exists in the local FastAPI app (`app/api/endpoints/estimate.py`) for
dev/testing only.

```
Photos
  → EXIF extraction          (FocalLengthIn35mmFilm → pixel focal length)
  → Grounding DINO           (open-vocabulary object detection, multi-prompt)
  → SAM 2.1 Hiera Large      (per-instance segmentation mask)
  → Depth Anything V2        (metric monocular depth map)
  → RE lookup                (primary: match against 74-item catalog)
  → Geometric OBB            (fallback: Open3D oriented bounding box)
  → Within-image dedup       (merge overlapping detections, same photo)
  → CLIP cross-image dedup   (cluster visually-similar crops across photos)
  → Qwen2-VL dedup           (VLM pass over the CLIP-reduced item set)
  → Packing multipliers      (only for geometric OBB items; RE volumes already include handling space)
  → DetectedItem[]
```

### RE (Raumeinheit) catalog

74 standardised furniture entries (`app/models/schemas.py::RE_CATALOG`), from
the Alltransport 24 Umzugsgutliste. `1 RE = 0.1 m³`. Mirrored in
`crates/volume-estimator/src/re_catalogue.txt` for the Rust-side VLM prompt —
kept in sync by hand, no shared source of truth.

| Type | Logic |
|------|-------|
| Fixed | Detect item → lookup RE value directly. Chair = 2 RE = 0.2 m³ |
| Size-variant | Detect → measure key dimension → pick RE bracket. Table ≤1.0 m = 5 RE, >1.2 m = 8 RE |
| Per-unit | Detect → measure width/length → count units. Sofa: width 2.1 m ÷ 0.65 m/seat ≈ 3 seats × 4 RE |

Items not in the catalog fall back to Depth Anything V2 + EXIF intrinsics for
geometric OBB estimation.

### Cross-image dedup — the known bottleneck

The same physical object photographed from multiple angles was, before the
CLIP/Qwen stages existed, counted once per photo — roughly **doubling** the
estimated volume. Crop-based CLIP + Qwen (comparing item thumbnails) narrows
this but doesn't fully solve it; a full-image VLM shown the whole photo set
at once (see Part B) came in at roughly **half** the crop-pipeline's count
in evaluation, closer to the human gold standard.

### Infrastructure

Modal `PhotoPipeline` (`gpu="L4"`), `scaledown_window=120`, `max_containers=1`,
`timeout=1800`. Fronted by the no-GPU `serve` function
(`scaledown_window=60`, `max_containers=2`, `timeout=60`).

### Fallback

If the ML service is unavailable or disabled, submission handling falls back
further down the priority order above (VLM, or LLM vision as a last resort
depending on config).

---

## 2. Video Pipeline

**Entry point**: Modal `serve_video` app, `POST /estimate/video/submit` +
`GET /estimate/video/status/{job_id}` (async). A synchronous
`POST /estimate/video` also exists locally (`app/api/endpoints/video.py`,
multipart, `≤500MB`, `video/*` content types) for dev/testing.

**Goal**: True metric 3D reconstruction of a room from a walkthrough video —
more accurate than monocular depth because multiple viewpoints are used.

```
Video
  → Keyframe extraction      (scene-change detection + blur rejection)
  → MASt3R                   (multi-view stereo 3D reconstruction → metric point cloud + camera poses)
  → Grounding DINO           (object detection on keyframes)
  → SAM 2 video predictor    (temporal mask propagation across all frames → no cross-image dedup needed)
  → Mask → point cloud proj  (project SAM masks onto MASt3R point cloud per object)
  → OBB fitting              (Open3D oriented bounding box per object)
  → RE lookup                (same catalog as photo pipeline)
  → DetectedItem[]
```

### Why MASt3R instead of monocular depth

Monocular depth (Depth Anything V2) estimates depth from a single image;
scale is approximate and depends on EXIF intrinsics being correct, and
degrades on unusual focal lengths or unknown camera models. MASt3R jointly
estimates a metric 3D point cloud and camera pose across all keyframes, so
geometry is consistent across the whole room rather than frame-by-frame.

### GPU memory management

MASt3R and the detection/segmentation models (DINO + SAM 2 + Depth Anything)
do not fit in VRAM simultaneously on an L4 (24 GB), so `model_loader.py`
loads MASt3R on demand and swaps it out for the detection stack before
running detection/segmentation — see `services/vision/app/vision/model_loader.py`.

Modal `VideoPipeline` (`gpu="L4"`), `scaledown_window=120`, `max_containers=1`,
`timeout=900`. Fronted by the no-GPU `serve_video` function
(`scaledown_window=60`, `max_containers=2`, `timeout=60`).

---

## Part B — Rust-side VLM Backend (production default)

`crates/volume-estimator/src/vlm.rs` (`VlmEstimator`) sends photos/frames to
an Ollama-hosted vision-language model along with the RE catalogue
(`re_catalogue.txt`, embedded via `include_str!`), and asks it to deduplicate,
classify, and volume-estimate in a single pass — no Modal call, no separate
detection/segmentation/depth stages.

- Selected via `vision_service.backend = "vlm"` in config
  (`crates/core/src/config.rs`).
- Model tag: `vision_service.vlm_model`; connects through
  `llm.ollama.base_url`/`api_key`.
- Thinking models are slow — a wall-clock timeout
  (`vision_service.vlm_timeout_secs`) exists because e.g. a large reasoning
  model can take minutes on a full photo set.
- This is the architecture validated by the eval scripts in
  `services/vision/vlm_ollama_eval.py` / `vlm_cloud_eval.py` (standalone
  experiments, not production code, run against a separate Modal app
  `aust-vlm-eval` or Ollama Cloud) — full-image VLM input beat the crop-based
  CLIP/Qwen pipeline on cross-image dedup accuracy, which is why this became
  the default backend.

## Part C — On-Device (LiDAR)

**App repo**: `app/` (in-repo git submodule; SvelteKit + Capacitor).

iPhones with LiDAR compute per-item volumes on-device (depth back-projection
+ oriented bounding box) during AR capture. If every manifest item the app
submits carries a plausible `device_volume_m3`, the backend takes those
values directly — `estimation_method = "ar_device"` — and does not call any
vision service.

Auth: customer magic-link OTP → session token. Tables: `customer_otps`,
`customer_sessions`. Routes: `POST /api/v1/customer/auth/*` (public),
`/api/v1/customer/*` (protected). Middleware:
`crates/api/src/middleware/customer_auth.rs` (DB-backed session token, not JWT).

---

## Output Format

All estimation paths converge on the same `DetectedItem` shape, stored in
`volume_estimations.result_data`. Fields (from
`services/vision/app/models/schemas.py::DetectedItem`):

```json
{
  "name": "sofa",
  "volume_m3": 1.2,
  "dimensions": { "length_m": 2.2, "width_m": 0.9, "height_m": 0.85 },
  "confidence": 0.92,
  "seen_in_images": [0, 1, 3],
  "category": "furniture",
  "german_name": "Sofa, Couch, Liege je Sitz",
  "re_value": 12.0,
  "units": 3,
  "volume_source": "re",
  "is_moveable": true
}
```

`total_volume_m3` on the inquiry is `SUM(item.volume_m3 × item.units)` and
flows directly into the pricing engine for offer generation.
