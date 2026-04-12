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

## Two Vision Approaches

### 1. ML Vision Service (`VisionServiceClient`)
- HTTP client for `services/vision/`
- Sends S3 keys → service downloads images and runs full pipeline
- Grounding DINO → SAM2 → Depth Anything V2 → OBB
- More accurate, GPU-heavy, ~3-10s per image on L4

### 2. LLM Vision (`VisionAnalyzer`)
- Sends base64 images to Claude/OpenAI with a prompt
- Fast, cheap, less accurate (no actual measurement)
- Used as **fallback** when ML service is unavailable

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

Uses `VisionServiceConfig` from core: `enabled`, `base_url`, `timeout_secs` (default 120), `max_retries` (default 1).