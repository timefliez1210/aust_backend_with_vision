# Vision Pipeline & Mobile App — Technical Reference

How the system estimates moving volume from images, video, and on-device depth capture.

---

## 1. Photo Pipeline

**Entry points**
- Customer photo webapp: `POST /api/v1/submit/photo` (multipart, no auth)
- Admin dashboard: `POST /api/v1/inquiries/{id}/estimate/depth`

**Goal**: Identify all furniture in a set of room photos and sum their volumes.

### Processing steps

```
Photos
  → EXIF extraction          (FocalLengthIn35mmFilm → pixel focal length)
  → Grounding DINO           (open-vocabulary object detection)
  → SAM 2.1 Hiera Large      (per-instance segmentation mask)
  → Depth Anything V2        (metric monocular depth map)
  → RE lookup                (primary: match against 73-item catalog)
  → Geometric OBB            (fallback: Open3D oriented bounding box)
  → Scale calibration        (EXIF intrinsics + depth map → real-world dims)
  → Within-image dedup       (remove duplicate detections in same photo)
  → Packing multipliers      (only for geometric OBB items; RE volumes already include handling space)
  → DetectedItem[]
```

### RE (Raumeinheit) catalog

73 standardised furniture volumes from the Alltransport 24 Umzugsgutliste.
`1 RE = 0.1 m³`.

| Type | Logic |
|------|-------|
| Fixed | Detect item → lookup RE value directly. Chair = 2 RE = 0.2 m³ |
| Size-variant | Detect → measure key dimension → pick RE bracket. Table ≤1.0 m = 5 RE, >1.2 m = 8 RE |
| Per-unit | Detect → measure width → count units. Sofa: width 2.1 m ÷ 0.65 m/seat ≈ 3 seats × 4 RE |

For items not in the catalog, Depth Anything V2 + EXIF intrinsics are used to estimate dimensions geometrically (OBB on the segmented depth region).

### Infrastructure

Deployed on Modal serverless GPU (L4, 24 GB VRAM).
Function `serve`: `max_inputs=4`, `max_containers=1`, 60 s idle shutdown, 1800 s timeout.
Typical latency: **~5 s per job**.

### Fallback

If the ML service is unavailable or disabled, the API falls back to LLM vision (Claude/OpenAI). The LLM receives base64 images and returns a structured item list. Less accurate (no actual measurement), but fast and cheap.

---

## 2. Video Pipeline

**Entry point**: Admin dashboard → `POST /api/v1/inquiries/{id}/estimate/video` (multipart `.mp4 / .mov / .webm / .mkv`, max 500 MB).

**Goal**: True metric 3D reconstruction of a room from a walkthrough video — more accurate than monocular depth because multiple viewpoints are used.

### Processing steps

```
Video
  → Keyframe extraction      (OpenCV: scene-change detection + blur rejection, 10–20 frames)
  → MASt3R                   (multi-view stereo 3D reconstruction → metric point cloud + camera poses)
  → Grounding DINO           (object detection on keyframes)
  → SAM 2 video predictor    (temporal mask propagation across all frames → no dedup needed)
  → Mask → point cloud proj  (project SAM masks onto MASt3R point cloud per object)
  → OBB fitting              (Open3D oriented bounding box per object)
  → RE lookup                (same catalog as photo pipeline)
  → Scale correction         (RE catalog anchors + ceiling height validation)
  → DetectedItem[]
```

### Why MASt3R instead of monocular depth

Monocular depth (Depth Anything V2) estimates depth from a single image. Scale is approximate and depends on EXIF intrinsics being correct. Accuracy degrades on unusual focal lengths or unknown camera models.

MASt3R solves multi-view stereo: given N keyframes, it jointly estimates a metric 3D point cloud and the camera pose for every frame. The geometry is consistent across the whole room, not just within a single image. Scale is anchored by RE catalog detections and ceiling height measurement.

### Scale anchoring

Two mechanisms prevent scale drift:

1. **RE catalog anchors**: High-confidence items with known RE values constrain the point cloud scale at solve time.
2. **Ceiling height validation**: Floor plane detection + distance to ceiling checks whether the MASt3R scale is physically plausible (typical room: 2.3–2.8 m).

If MASt3R produces fewer than 1000 points or the scale fails validation, the pipeline **falls back to per-keyframe Depth Anything V2** (same as photo pipeline).

### GPU memory management

MASt3R (~1.5 GB weights, ~12–15 GB peak) and the detection/segmentation models (~3 GB combined) cannot fit in VRAM simultaneously. They are loaded and unloaded in phases:

| Phase | Models on GPU | Peak VRAM |
|-------|--------------|-----------|
| Startup / idle | DINO + SAM 2 + DA | ~3 GB |
| Phase 1: reconstruction | MASt3R only | ~12–15 GB |
| Phase 2: detection/segmentation | DINO + SAM 2 | ~5 GB |
| Phase 3: OBB fitting | CPU only (Open3D) | ~0 GB |

Function `serve_video`: `max_inputs=1`, `max_containers=1`, 120 s idle shutdown, 1800 s timeout.
Typical latency: **2–10 min per job** depending on video length.

---

## 3. Mobile App — On-Device 3D Reconstruction

**Repo**: `alex_aust_app/` (SvelteKit + Capacitor)
**Plugin**: `plugins/capacitor-depth-capture/` (TypeScript + Swift/ARKit + Java/ARCore)

### Concept

The phone captures a sequence of RGBD frames while the user slowly walks through the room. Each frame includes:

- **RGB image** — JPEG, for object detection
- **Depth map** — 16-bit PNG, mm precision (LiDAR on iPhone Pro; estimated depth on others)
- **Camera intrinsics** — fx, fy, cx, cy in pixels
- **Camera pose** — 4×4 transform matrix from the AR session (device-to-world)

The pose is the key piece. ARKit (`frame.camera.transform`) and ARCore (`camera.getPose()`) provide a calibrated, metric, visual-inertial odometry pose for every frame — for free. This gives the exact position and orientation of the camera in world space at each capture.

### Why camera poses matter

Without poses you have isolated depth frames — each one in its own coordinate system, impossible to merge. With poses you can project every depth frame into a shared world coordinate system and accumulate a single metric point cloud of the whole room. This is the same principle as MASt3R for video, except:

| | MASt3R (video pipeline) | Mobile AR |
|---|---|---|
| Poses | Estimated from image pairs | Given by ARKit/ARCore VIO (free, metric) |
| Depth | Estimated by stereo matching | LiDAR / ARCore depth API (direct measurement) |
| Scale | Estimated, needs anchoring | Metric from hardware |
| Latency | 2–10 min server-side | ~seconds (mostly upload + server fusion) |

The mobile approach is fundamentally simpler and more accurate because both depth and pose come directly from the hardware.

### Data flow

```
User scans room with phone
  → AR session running (ARWorldTrackingConfiguration / ARCore session)
  → captureFrame() called N times (e.g. every 0.5 s, ~20–40 frames total)
      returns: { imageBase64, depthMapBase64, width, height, intrinsics, transform }
  → frames buffered in app
  → user confirms scan
  → POST /api/v1/submit/mobile (multipart)
      fields per frame: image, depth_map, intrinsics (JSON), transform (JSON, 4×4 flat)
  → backend: project each depth map into world space using its pose
  → merge into single point cloud
  → run DINO detection on RGB frames
  → SAM 2 segment + project onto point cloud
  → OBB fitting → RE lookup → volume sum
  → inquiry + estimation record created
  → offer auto-generated
```

### Platform depth sources

| Platform | Depth source | Notes |
|----------|-------------|-------|
| iOS (LiDAR) | `frame.sceneDepth.depthMap` | iPhone 12 Pro+, iPad Pro. Accurate to ~1% at 5 m. |
| iOS (no LiDAR) | `frame.smoothedSceneDepth.depthMap` | ARKit photogrammetry estimate. Less accurate, but usable. |
| Android (ARCore) | `Frame.acquireDepthImage16Bits()` | Available on ARCore Depth API devices. |
| Web fallback | None | Falls back to photo pipeline (no depth, no pose). |

### Current implementation status

The plugin currently captures: **RGB + depth map + intrinsics**.
**Camera pose (`frame.camera.transform`) is NOT yet included.**

To complete the implementation, add to each layer:

1. **`definitions.ts`** — add `transform: number[]` (16 floats, row-major 4×4) to `CapturedFrame`
2. **Swift (iOS)** — `frame.camera.transform` is `simd_float4x4`; flatten columns to a 16-element float array and include in `call.resolve()`
3. **Android** — `camera.getPose().toMatrix(float[], 0)` returns a 16-element float array
4. **Backend `submit/mobile` handler** — parse `transform` per frame, project depth maps to world space, merge point cloud, run volume estimation

### Auth

Customer magic-link OTP → session token. Tables: `customer_otps`, `customer_sessions`.
Routes: `POST /api/v1/customer/auth/request` → `POST /api/v1/customer/auth/verify` → session token → protected `/api/v1/customer/*`.

---

## 4. Output Format

All three pipelines produce the same `DetectedItem` structure stored in `volume_estimations.result_data` (raw JSON array):

```json
[
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
    "crop_s3_key": "estimates/{inquiry_id}/{estimation_id}/crops/sofa_0.jpg"
  }
]
```

`total_volume_m3` on the inquiry is set from `SUM(item.volume_m3 × item.units)` and flows directly into the pricing engine for offer generation.
