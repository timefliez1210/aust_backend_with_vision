# services/vision — 3D Volume Estimation (Python ML Service)

Standalone Python microservice that estimates furniture volumes from room photos using a GPU-accelerated ML pipeline.

## Architecture

```
modal_app.py              → Modal serverless deployment (T4 GPU)
app/main.py               → FastAPI application + lifespan (model loading)
app/api/router.py          → HTTP route registration
app/api/endpoints/
  health.py               → GET /health, GET /ready
  estimate.py             → POST /estimate/images (S3-based)
app/vision/
  pipeline.py             → Orchestrator: detect → segment → depth → volume → dedup
  detector.py             → Grounding DINO (open-vocabulary object detection)
  segmenter.py            → SAM (instance segmentation)
  depth.py                → Depth Anything V2 (monocular depth estimation)
  volume.py               → 3D point cloud → OBB volume + scale calibration
  model_loader.py         → ModelRegistry singleton, weight download, warm-up
app/models/schemas.py      → Pydantic types + reference dimension catalog
app/storage/s3_client.py   → boto3 S3 client for image download
app/config.py              → Settings via pydantic-settings (VISION_ env prefix)
```

## ML Pipeline

```
Images → Grounding DINO (detect objects)
           ↓
         SAM (segment each detection)
           ↓
         Depth Anything V2 (estimate per-pixel depth)
           ↓
         Open3D (back-project to 3D, compute OBB volume)
           ↓
         Scale Calibration (ground monocular depth in metric units)
           ↓
         Cross-image Deduplication (merge same item from multiple angles)
           ↓
         Response (items with volumes, dimensions, confidence)
```

## Scale Calibration

Monocular depth is relative, not metric. To convert to real-world units:
1. After raw OBB computation, find the highest-confidence detection with a known reference size
2. Compare estimated largest dimension vs reference largest dimension (e.g., sofa → 2.10m, chair → 0.85m)
3. Apply linear scale factor to all items in the image

Reference dimensions for ~30 common items are defined in `app/models/schemas.py:REFERENCE_DIMENSIONS`.

## Deployment

### Modal (Production)

```bash
modal deploy services/vision/modal_app.py
```

- GPU: T4
- Idle shutdown: 60 seconds (cost-efficient for 1-2 jobs/day)
- Max containers: 1
- Concurrent requests: 4
- Model weights baked into image (no download at startup)
- URL: `https://crfabig--aust-vision-serve.modal.run`

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
```json
{"status": "ready", "models_loaded": true, "gpu_available": true}
```

### POST /estimate/upload (Modal)
Multipart form upload for direct testing.
```bash
curl -X POST .../estimate/upload \
  -F "job_id=test-1" \
  -F "images=@room1.jpg" -F "images=@room2.jpg"
```

### POST /estimate/images (Docker)
JSON request with S3 keys:
```json
{
  "job_id": "uuid",
  "s3_keys": ["estimates/quote-id/est-id/room1.jpg"],
  "options": {"detection_threshold": 0.3}
}
```

### Response Format
```json
{
  "job_id": "uuid",
  "status": "completed",
  "detected_items": [
    {
      "name": "Sofa",
      "volume_m3": 1.85,
      "dimensions": {"length_m": 2.2, "width_m": 0.9, "height_m": 0.85},
      "confidence": 0.92,
      "seen_in_images": [0, 1],
      "category": "furniture"
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
| `VISION_DEPTH_SCALE` | `1.0` | Depth normalization multiplier |
| `VISION_S3_ENDPOINT` | `http://minio:9000` | MinIO/S3 URL |
| `VISION_S3_BUCKET` | `aust-uploads` | Image bucket |
| `VISION_S3_ACCESS_KEY` | `minioadmin` | S3 credentials |
| `VISION_S3_SECRET_KEY` | `minioadmin` | S3 credentials |

## Benchmark Results

Tested on 30 images (HuggingFace furniture-ngpea dataset + known-dimension items):

- **Detection accuracy**: 97% (29/30 items correctly identified)
- **Volume accuracy** (HF dataset, same-category items): chairs 0.90x real, tables 0.99x real
- **Volume accuracy** (known-dimension items): median ~50% error, 62% within 50%
- Best on standard furniture (chairs, desks, pianos: <35% error)
- Worst on flat/thin items (mattresses, bicycles) and product photos with no environmental depth context

Accuracy improves significantly with real room photos vs isolated product photos.

## Models Used

| Model | Size | Purpose |
|-------|------|---------|
| Grounding DINO (IDEA-Research/grounding-dino-base) | ~900MB | Object detection |
| SAM ViT-H (sam_vit_h_4b8939.pth) | ~2.5GB | Segmentation |
| Depth Anything V2 Small (depth-anything/Depth-Anything-V2-Small-hf) | ~100MB | Depth estimation |
