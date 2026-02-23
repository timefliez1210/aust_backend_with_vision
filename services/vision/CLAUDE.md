# services/vision — Volume Estimation (Python ML Service)

Standalone Python microservice that estimates furniture volumes from room photos and videos using GPU-accelerated ML pipelines with RE (Raumeinheit) lookup.

## Architecture

```
modal_app.py              → Modal serverless deployment (L4 GPU, photo + video functions)
app/main.py               → FastAPI application + lifespan (model loading)
app/api/router.py          → HTTP route registration
app/api/endpoints/
  health.py               → GET /health, GET /ready
  estimate.py             → POST /estimate/images (S3-based)
  video.py                → POST /estimate/video (multipart video upload)
app/vision/
  pipeline.py             → Photo orchestrator: detect → segment → depth → volume → dedup
  video_pipeline.py       → Video orchestrator: keyframes → MASt3R → DINO → SAM 2 → volume
  keyframe.py             → OpenCV keyframe extraction (scene change + blur rejection)
  reconstructor.py        → MASt3R multi-view 3D reconstruction
  video_segmenter.py      → SAM 2 video predictor (temporal tracking)
  detector.py             → Grounding DINO (open-vocabulary object detection)
  segmenter.py            → SAM 2 image predictor (instance segmentation)
  depth.py                → Depth Anything V2 (monocular depth estimation)
  volume.py               → RE lookup + EXIF intrinsics + OBB fallback
  model_loader.py         → ModelRegistry with GPU memory management
app/models/schemas.py      → Pydantic types + RE catalog + reference dimensions
app/storage/s3_client.py   → boto3 S3 client for image download
app/config.py              → Settings via pydantic-settings (VISION_ env prefix)
```

## Two Pipelines

### Photo Pipeline (existing)
```
Images → EXIF extraction → DINO detect → SAM 2 segment → Depth Anything V2
       → RE lookup / geometric OBB → scale calibration → dedup → packing multipliers
```
- Uses monocular depth (Depth Anything V2) for dimension estimation
- Within-image dedup only (cross-image unreliable with single-view features)
- ~5s per job on L4

### Video Pipeline (new)
```
Video → keyframe extraction (OpenCV)
      → MASt3R 3D reconstruction (metric point cloud + camera poses)
      → DINO detect → SAM 2 video tracking (temporal propagation)
      → mask → point cloud projection → OBB fitting
      → RE lookup / geometric OBB → scale correction → packing multipliers
```
- True multi-view 3D via MASt3R (not monocular depth)
- SAM 2 temporal tracking eliminates deduplication problems
- Scale correction via RE catalog anchors + ceiling height validation
- 2-10 min per job on L4

### GPU Memory Management

Models are swapped between phases to stay within 24GB L4 VRAM:

| Phase | Models on GPU | Peak VRAM |
|-------|--------------|-----------|
| Startup/idle | DINO + SAM 2 + DA | ~3GB |
| Photo pipeline | DINO + SAM 2 + DA active | ~8GB |
| Video Phase 1 | MASt3R only (detection → CPU) | ~12-15GB |
| Video Phase 2 | DINO + SAM 2 (MASt3R unloaded) | ~5GB |
| Video Phase 3 | CPU only (Open3D) | ~0 |

## Volume Estimation Strategy

### Hybrid: RE Lookup + Geometric Fallback

**Primary: RE (Raumeinheit) Catalog** — 73 items from the Alltransport 24 Umzugsgutliste (1 RE = 0.1 m³).

| Type | How it works | Example |
|------|-------------|---------|
| Fixed | Detect → lookup RE | Chair = 2 RE = 0.2 m³ |
| Size variant | Detect → measure dimension → pick RE bracket | Table ≤1.0m = 5 RE, >1.2m = 8 RE |
| Per-unit | Detect → measure width → count units | Sofa: width 2.1m ÷ 0.65m/seat ≈ 3 seats × 4 RE = 1.2 m³ |

**Fallback: Geometric OBB** — For items not in the RE catalog, uses Open3D OBB with scale calibration.

### Video Pipeline Robustness

- **Mask erosion**: SAM 2 masks eroded by 2px before point cloud projection to avoid boundary noise
- **RE scale correction**: High-confidence RE catalog detections anchor the point cloud scale
- **Ceiling height validation**: Floor plane detection + ceiling measurement validates MASt3R scale
- **Graceful fallback**: If MASt3R fails (< 1000 points, bad scale), falls back to per-keyframe Depth Anything V2

## Deployment

### Modal (Production)

```bash
modal deploy services/vision/modal_app.py
```

Two separate Modal functions (same image, different concurrency):

| Function | GPU | max_inputs | Use case |
|----------|-----|-----------|----------|
| `serve` (photo) | L4 | 4 | Fast photo processing (~5s) |
| `serve_video` | L4 | 1 | Long video processing (2-10 min) |

Both have `max_containers=1` and idle shutdown (60s photo, 120s video).

### Docker (Local Dev)

```bash
# CPU mode
docker compose -f docker/docker-compose.yml up vision

# GPU mode
docker compose -f docker/docker-compose.yml -f docker/docker-compose.gpu.yml up vision
```

## API

### GET /health
Always returns `{"status": "ok"}`.

### GET /ready
Returns 200 if models loaded, 503 if still loading.

### POST /estimate/upload (Photo)
Multipart form upload for photo estimation.
```bash
curl -X POST .../estimate/upload \
  -F "job_id=test-1" \
  -F "images=@room1.jpg" -F "images=@room2.jpg"
```

### POST /estimate/video (Video)
Multipart video upload for 3D volume estimation.
```bash
curl -X POST .../estimate/video \
  -F "job_id=test-1" \
  -F "video=@room_walkthrough.mp4" \
  -F "max_keyframes=20" \
  -F "detection_threshold=0.3"
```
- Accepts: mp4, mov, webm, mkv (max 500MB)
- Processing: 2-10 minutes depending on video length

### POST /estimate/images (Docker)
JSON request with S3 keys.

### Response Format (same for photo and video)
```json
{
  "job_id": "uuid",
  "status": "completed",
  "detected_items": [
    {
      "name": "sofa",
      "volume_m3": 1.2,
      "dimensions": {"length_m": 2.2, "width_m": 0.9, "height_m": 0.85},
      "confidence": 0.92,
      "seen_in_images": [0, 1, 3, 5],
      "category": "furniture",
      "german_name": "Sofa, Couch, Liege je Sitz",
      "re_value": 12.0,
      "units": 3,
      "volume_source": "re"
    }
  ],
  "total_volume_m3": 8.45,
  "confidence_score": 0.85,
  "processing_time_ms": 4250
}
```

## Configuration (VISION_ env prefix)

| Variable | Default | Purpose |
|----------|---------|---------|
| `VISION_DEVICE` | `cuda` | `cuda` or `cpu` |
| `VISION_WEIGHTS_DIR` | `/app/weights` | Model weights path |
| `VISION_DETECTION_THRESHOLD` | `0.3` | Grounding DINO confidence threshold |
| `VISION_DEDUP_SIMILARITY_THRESHOLD` | `0.85` | Cross-image dedup threshold |
| `VISION_VIDEO_MAX_KEYFRAMES` | `20` | Max keyframes extracted from video |
| `VISION_VIDEO_MIN_KEYFRAMES` | `10` | Min keyframes (uniform fill if fewer) |
| `VISION_VIDEO_BLUR_THRESHOLD` | `100.0` | Laplacian variance blur threshold |
| `VISION_VIDEO_MAX_SIZE_MB` | `500` | Max video upload size |
| `VISION_MAST3R_MIN_CONF` | `1.5` | MASt3R point confidence threshold |
| `VISION_MAST3R_ALIGNMENT_ITERS` | `300` | Global alignment iterations |
| `VISION_S3_ENDPOINT` | `http://minio:9000` | MinIO/S3 URL |
| `VISION_S3_BUCKET` | `aust-uploads` | Image bucket |
| `VISION_S3_ACCESS_KEY` | `minioadmin` | S3 credentials |
| `VISION_S3_SECRET_KEY` | `minioadmin` | S3 credentials |

## Models Used

| Model | Size | Purpose |
|-------|------|---------|
| Grounding DINO (IDEA-Research/grounding-dino-base) | ~900MB | Object detection |
| SAM 2.1 Hiera Large (facebook/sam2.1-hiera-large) | ~900MB | Segmentation + video tracking |
| Depth Anything V2 Metric Indoor Large | ~1.3GB | Metric depth (photo pipeline) |
| MASt3R ViTLarge (naver/MASt3R_ViTLarge_BaseDecoder_512) | ~1.5GB | 3D reconstruction (video pipeline) |
