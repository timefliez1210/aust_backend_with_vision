# services/vision — Python ML Pipeline (GPU)

FastAPI service for 3D volume estimation from photos, depth maps, and video.

## Architecture

```
POST /estimate/upload     → photo pipeline (GroundingDINO + SAM2 + depth)
POST /estimate/depth      → depth sensor pipeline (LiDAR + DepthAnything fusion)
POST /estimate/video      → video pipeline (MASt3R + SAM2)
POST /estimate/ar/submit  → AR per-item pipeline (DINO prompt + SAM2 + MASt3R + DBSCAN)
GET  /estimate/status/{id} → polling endpoint
GET  /health              → liveness check
```

## Key Files

| File | Purpose |
|------|---------|
| `app/main.py` | FastAPI app, route wiring |
| `app/pipeline.py` | Photo/depth/video pipeline orchestration |
| `app/models/schemas.py` | Pydantic request/response models, `RE_CATALOG` German Umzugsgutliste |
| `app/vision/grounding_dino.py` | Object detection with German prompts |
| `app/vision/sam2_mask.py` | Segmentation masks |
| `app/vision/depth_anything.py` | Monocular depth estimation |
| `app/vision/mast3r_recon.py` | Multi-view stereo reconstruction |
| `app/volume.py` | OBB volume calculation from point clouds |
| `modal_app.py` | Modal deployment (serverless GPU L4) |

## Estimation Method Mapping

| Method | `estimation_method` string | Pipeline |
|--------|--------------------------|----------|
| Photo upload | `vision` | GroundingDINO → SAM2 → DepthAnything → OBB |
| Depth sensor | `depth_sensor` | DepthAnything (with LiDAR fusion if available) |
| AR per-item | `ar` | DINO prompt → SAM2 → MASt3R → DBSCAN → OBB |
| Video | `video` | MASt3R + SAM2 multi-view |
| Manual | `manual` | No ML, customer-provided volume |
| Inventory form | `inventory` | Parsed from VolumeCalculator items list |

## Deployment

**Local**: `cd services/vision && uvicorn app.main:app --port 8001`
**Production**: `modal deploy modal_app.py` → serverless GPU

The Rust backend calls this via `aust_volume_estimator::VisionServiceClient`.