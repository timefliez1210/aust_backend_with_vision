# crates/volume-estimator — Volume Calculation

> Pipeline context (how volume feeds into pricing): [../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md)
> Two separate hours systems (offer hours vs payroll hours): [../../docs/ARCHITECTURE.md#two-separate-hours-systems](../../docs/ARCHITECTURE.md)

Calculates moving volume from images (LLM vision or 3D ML pipeline), manual inventory lists, or depth sensor data.

## Key Files

- `src/vision.rs` - LLM-based image analysis (VisionAnalyzer)
- `src/vision_service.rs` - HTTP client for Python ML service (VisionServiceClient)
- `src/inventory.rs` - Manual inventory processing
- `src/calculator.rs` - Volume calculation utilities
- `src/error.rs` - VolumeError enum

## Two Vision Approaches

### 1. LLM Vision (`VisionAnalyzer`)
- Sends base64 images to Claude/OpenAI with a prompt
- LLM identifies items and estimates volumes from visual inspection
- Fast, cheap, but less accurate (no actual measurement)
- Used as **fallback** when ML service is unavailable

### 2. ML Vision Service (`VisionServiceClient`)
- HTTP client for the Python sidecar (`services/vision/`)
- Sends S3 keys → service downloads images and runs full pipeline
- Grounding DINO → SAM → Depth Anything V2 → Open3D TSDF/OBB
- More accurate, GPU-heavy, ~3-10s per image on T4

### Fallback Pattern
The API endpoint tries the ML service first. If it's disabled, unreachable, or errors, it falls back to LLM vision automatically.

## VisionServiceClient

```rust
let client = VisionServiceClient::new(base_url, timeout_secs, max_retries);
let ready = client.check_ready().await?;        // GET /ready
let result = client.estimate_images(req).await?; // POST /estimate/images
```

- Retries with exponential backoff (2^n seconds) on 5xx errors
- Request: `VisionServiceRequest { job_id, s3_keys, options }`
- Response: `VisionServiceResponse { job_id, status, detected_items, total_volume_m3, confidence_score, processing_time_ms }`

## Error Variants

`VolumeError`: Vision, Inventory, Llm, Storage, ExternalService(String), InvalidData

## Configuration

Uses `VisionServiceConfig` from core:
- `enabled` — whether to try ML service (default: false)
- `base_url` — service URL (default: http://localhost:8090)
- `timeout_secs` — HTTP timeout (default: 120, ML is slow)
- `max_retries` — retry count on 5xx (default: 1)
