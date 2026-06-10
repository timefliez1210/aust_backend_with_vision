//! VLM-first volume estimation: full apartment photos + RE catalogue in a
//! single vision-model pass via Ollama Cloud.
//!
//! Replaces the crop-based GPU pipeline (Modal: DINO → SAM2 → depth → CLIP/Qwen
//! dedup) for photo and video estimation. Benchmark on the 59-photo reference
//! set (gold standard ~37 m³): minimax-m3 32.6 m³ (furniture essentially at
//! gold), vs 60.5 m³ (~1.9× over) for the crop pipeline. The model sees whole
//! rooms, so cross-photo deduplication — the crop pipeline's unfixable
//! weakness — happens with scene context.
//!
//! Video is handled by extracting keyframes with ffmpeg and running the same
//! photo path (Ollama's API takes images only).

use crate::vision::extract_json;
use crate::VolumeError;
use aust_core::models::{DetectedItem, VisionAnalysisResult};
use aust_llm_providers::{LlmMessage, OllamaProvider};
use base64::Engine;
use serde::Deserialize;
use std::time::Duration;

/// `english_key | German name | volume | flags` lines generated from the
/// Python source of truth (`services/vision/app/models/schemas.py::RE_CATALOG`,
/// rendered by `services/vision/vlm_cloud_eval.py::build_catalogue()`).
/// Regenerate this file when the catalogue changes.
const RE_CATALOGUE: &str = include_str!("re_catalogue.txt");

/// Maximum edge length for downscaled images sent to the model.
/// 512 px matches the benchmarked configuration.
const MAX_IMAGE_DIM: u32 = 512;

/// Maximum number of keyframes extracted from a video.
const MAX_VIDEO_FRAMES: usize = 40;

/// Fixed confidence reported for VLM estimations. The model does not produce
/// a calibrated self-score; this keeps downstream display/threshold logic fed.
const VLM_CONFIDENCE: f64 = 0.7;

/// 1 RE = 0.1 m³ (German moving industry standard, Umzugskarton ≈ 1 RE).
const KARTON_M3: f64 = 0.1;

fn build_prompt() -> String {
    format!(
        r#"You are estimating moving volume for a German moving company, working end-to-end as the whole pipeline.

You are given photos of ONE apartment, taken from many angles for a moving quote, plus the RE volume catalogue below. Each catalogue line is `english_key | German name | volume | flags`. Volume may be fixed "X m3", a "size-variant A-B m3" range, or "X m3 per seat/meter". Flag NOT-MOVED = stays with property (exclude from total). Flag BOX = small, packed into moving boxes (Umzugskarton = 0.1 m3 each).

CATALOGUE:
{RE_CATALOGUE}

TASK — using ALL the photos:
- DEDUPLICATE across photos: the same physical object shown in several photos is ONE object. Use whole-room context to decide same-vs-different. Do NOT list the same object under two catalogue synonyms (e.g. a bed is ONE entry, not bed + mattress; a rug is rug OR carpet, not both).
- For each distinct movable object: pick the best-matching catalogue key, take its volume (for "per seat/meter" estimate the count; for size-variants pick small/large by what you see). Count quantities per type carefully, room by room.
- EXCLUDE NOT-MOVED items from the total; list them separately. Built-in kitchen units (Einbaukueche), built-in oven/dishwasher, radiators stay with the property.
- For BOX items and loose contents (full wardrobes, kitchen contents, books, clothes), estimate how many Umzugskartons (a full wardrobe is typically 8-12 Kartons).

Respond ONLY with a single JSON object, no markdown, no explanation:
{{
  "movable_items": [
    {{"key": "<english_key>", "german": "<German name>", "count": <int>, "volume_per_unit_m3": <float>, "volume_m3": <float line total>}}
  ],
  "boxes_estimate": <int total Umzugskartons>,
  "not_moved": [{{"item": "<name>", "reason": "<short reason>"}}],
  "total_movable_m3": <float>
}}"#
    )
}

/// Raw JSON shape returned by the model.
#[derive(Debug, Deserialize)]
struct VlmResponse {
    #[serde(default)]
    movable_items: Vec<VlmItem>,
    #[serde(default)]
    boxes_estimate: u32,
    #[serde(default)]
    not_moved: Vec<VlmNotMoved>,
}

#[derive(Debug, Deserialize)]
struct VlmItem {
    #[serde(default)]
    key: String,
    #[serde(default)]
    german: Option<String>,
    #[serde(default = "default_count")]
    count: u32,
    #[serde(default)]
    volume_per_unit_m3: Option<f64>,
    #[serde(default)]
    volume_m3: Option<f64>,
}

fn default_count() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
struct VlmNotMoved {
    #[serde(default)]
    item: String,
    #[serde(default)]
    reason: String,
}

/// Catalogue-grounded VLM volume estimator (photos and video keyframes).
///
/// **Caller**: `process_submission_background` / `process_video_background` in
/// `crates/api` when `vision_service.backend = "vlm"`.
pub struct VlmEstimator {
    provider: OllamaProvider,
    timeout: Duration,
}

impl VlmEstimator {
    /// # Parameters
    /// - `base_url` — Ollama server, e.g. `https://ollama.com` (cloud).
    /// - `api_key` — Bearer token; required for Ollama Cloud.
    /// - `model` — vision-capable model tag, e.g. `minimax-m3`.
    /// - `timeout_secs` — wall-clock generation ceiling. Thinking models need
    ///   a lot: minimax-m3 takes ~14 min on a 59-photo set.
    pub fn new(
        base_url: String,
        api_key: Option<String>,
        model: String,
        timeout_secs: u64,
    ) -> Self {
        let provider = match api_key {
            Some(key) if !key.is_empty() => {
                OllamaProvider::with_api_key(base_url, model, key)
            }
            _ => OllamaProvider::new(base_url, model),
        };
        Self {
            provider,
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    /// Run the catalogue estimation over a set of photos.
    ///
    /// Images are downscaled to ≤512 px JPEG before upload (the benchmarked
    /// configuration; full-resolution adds tokens without measured gain).
    pub async fn estimate_photos(
        &self,
        images: &[(Vec<u8>, String)],
    ) -> Result<VisionAnalysisResult, VolumeError> {
        if images.is_empty() {
            return Err(VolumeError::Vision("no images provided".into()));
        }

        let mut images_b64 = Vec::with_capacity(images.len());
        for (data, mime) in images {
            match decode_and_downscale(data).await {
                Ok(jpeg) => images_b64
                    .push(base64::engine::general_purpose::STANDARD.encode(jpeg)),
                Err(e) => {
                    tracing::warn!("Skipping undecodable image ({mime}): {e}");
                }
            }
        }
        if images_b64.is_empty() {
            return Err(VolumeError::Vision("no decodable images".into()));
        }

        let n_images = images_b64.len();
        let messages =
            vec![LlmMessage::user_with_images(build_prompt(), images_b64)];

        tracing::info!(n_images, "VLM estimation: submitting to Ollama");
        let t0 = std::time::Instant::now();
        let response = self
            .provider
            .complete_streaming(&messages, self.timeout)
            .await
            .map_err(|e| VolumeError::Llm(e.to_string()))?;
        tracing::info!(
            n_images,
            elapsed_s = t0.elapsed().as_secs(),
            response_chars = response.len(),
            "VLM estimation: response received"
        );

        parse_vlm_response(&response)
    }

    /// Run the catalogue estimation over a video by extracting keyframes
    /// with ffmpeg and reusing the photo path.
    pub async fn estimate_video(
        &self,
        video_bytes: &[u8],
    ) -> Result<VisionAnalysisResult, VolumeError> {
        let frames = extract_keyframes(video_bytes, MAX_VIDEO_FRAMES).await?;
        tracing::info!(n_frames = frames.len(), "VLM video: keyframes extracted");
        let images: Vec<(Vec<u8>, String)> = frames
            .into_iter()
            .map(|f| (f, "image/jpeg".to_string()))
            .collect();
        self.estimate_photos(&images).await
    }
}

/// Decode an image to a ≤512px JPEG, falling back to `heif-convert` for
/// HEIC/HEIF/AVIF (iPhone default format — the `image` crate cannot decode it;
/// the old Modal pipeline used pillow-heif). `heif-convert` comes from the
/// `libheif-examples` package in `docker/Dockerfile.backend`.
async fn decode_and_downscale(data: &[u8]) -> Result<Vec<u8>, VolumeError> {
    match downscale_to_jpeg(data, MAX_IMAGE_DIM) {
        Ok(jpeg) => Ok(jpeg),
        Err(rust_err) => match heic_to_jpeg(data).await {
            Ok(converted) => downscale_to_jpeg(&converted, MAX_IMAGE_DIM),
            Err(heif_err) => Err(VolumeError::Vision(format!(
                "{rust_err}; heif fallback: {heif_err}"
            ))),
        },
    }
}

/// Convert HEIC/HEIF/AVIF bytes to JPEG via the `heif-convert` CLI.
async fn heic_to_jpeg(data: &[u8]) -> Result<Vec<u8>, VolumeError> {
    let dir = tempfile::tempdir()
        .map_err(|e| VolumeError::Vision(format!("tempdir failed: {e}")))?;
    let input = dir.path().join("input.heic");
    let output = dir.path().join("output.jpg");
    tokio::fs::write(&input, data)
        .await
        .map_err(|e| VolumeError::Vision(format!("heic write failed: {e}")))?;

    let result = tokio::process::Command::new("heif-convert")
        .arg("-q")
        .arg("90")
        .arg(&input)
        .arg(&output)
        .output()
        .await
        .map_err(|e| VolumeError::Vision(format!("heif-convert spawn failed: {e}")))?;
    if !result.status.success() {
        return Err(VolumeError::Vision(format!(
            "heif-convert failed: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }

    // Multi-image HEIF containers produce output-1.jpg instead of output.jpg.
    let path = if output.exists() {
        output
    } else {
        dir.path().join("output-1.jpg")
    };
    tokio::fs::read(&path)
        .await
        .map_err(|e| VolumeError::Vision(format!("heif output read failed: {e}")))
}

/// Decode an image (JPEG/PNG/WebP), shrink to fit `max_dim`, re-encode JPEG q85.
fn downscale_to_jpeg(data: &[u8], max_dim: u32) -> Result<Vec<u8>, VolumeError> {
    let img = image::load_from_memory(data)
        .map_err(|e| VolumeError::Vision(format!("image decode failed: {e}")))?;
    let img = if img.width() > max_dim || img.height() > max_dim {
        img.thumbnail(max_dim, max_dim)
    } else {
        img
    };
    let mut buf = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 85);
    img.to_rgb8()
        .write_with_encoder(encoder)
        .map_err(|e| VolumeError::Vision(format!("jpeg encode failed: {e}")))?;
    Ok(buf)
}

/// Parse the model's JSON answer and rebuild the totals server-side.
///
/// The model's own `total_movable_m3` is ignored — line volumes plus the
/// Karton estimate are summed here so arithmetic slips can't reach pricing.
fn parse_vlm_response(response: &str) -> Result<VisionAnalysisResult, VolumeError> {
    let json_str = extract_json(response).unwrap_or(response);
    let parsed: VlmResponse = serde_json::from_str(json_str).map_err(|e| {
        tracing::warn!(
            "Failed to parse VLM response as JSON: {e}\nResponse: {:.2000}",
            response
        );
        VolumeError::Vision(format!("unparseable VLM response: {e}"))
    })?;

    let mut detected_items = Vec::with_capacity(parsed.movable_items.len() + 1);
    let mut total = 0.0_f64;

    for item in &parsed.movable_items {
        let count = item.count.max(1);
        let line_volume = item
            .volume_m3
            .or_else(|| item.volume_per_unit_m3.map(|v| v * count as f64))
            .unwrap_or(0.0);
        if line_volume <= 0.0 {
            tracing::warn!("VLM item without volume skipped: {:?}", item.key);
            continue;
        }
        total += line_volume;

        let german = item
            .german
            .clone()
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| item.key.clone());
        let name = if count > 1 {
            format!("{count}× {german}")
        } else {
            german.clone()
        };
        detected_items.push(DetectedItem {
            name,
            volume_m3: round2(line_volume),
            confidence: VLM_CONFIDENCE,
            dimensions: None,
            category: None,
            german_name: Some(german),
            re_value: Some(round2(line_volume / KARTON_M3)),
            volume_source: Some("re_catalog".to_string()),
            bbox: None,
            bbox_image_index: None,
            crop_s3_key: None,
            seen_in_images: None,
        });
    }

    if parsed.boxes_estimate > 0 {
        let karton_volume = parsed.boxes_estimate as f64 * KARTON_M3;
        total += karton_volume;
        detected_items.push(DetectedItem {
            name: format!("{}× Umzugskarton", parsed.boxes_estimate),
            volume_m3: round2(karton_volume),
            confidence: VLM_CONFIDENCE,
            dimensions: None,
            category: Some("boxes".to_string()),
            german_name: Some("Umzugskarton bis 80 l".to_string()),
            re_value: Some(parsed.boxes_estimate as f64),
            volume_source: Some("re_catalog".to_string()),
            bbox: None,
            bbox_image_index: None,
            crop_s3_key: None,
            seen_in_images: None,
        });
    }

    if detected_items.is_empty() {
        return Err(VolumeError::Vision(
            "VLM returned no movable items".into(),
        ));
    }

    let analysis_notes = if parsed.not_moved.is_empty() {
        None
    } else {
        Some(format!(
            "Nicht umgezogen (verbleibt in der Wohnung): {}",
            parsed
                .not_moved
                .iter()
                .map(|n| {
                    if n.reason.is_empty() {
                        n.item.clone()
                    } else {
                        format!("{} ({})", n.item, n.reason)
                    }
                })
                .collect::<Vec<_>>()
                .join("; ")
        ))
    };

    Ok(VisionAnalysisResult {
        detected_items,
        total_volume_m3: round2(total),
        confidence_score: VLM_CONFIDENCE,
        room_type: None,
        analysis_notes,
    })
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Extract up to `max_frames` evenly spaced keyframes from a video via ffmpeg.
///
/// Probes the duration with ffprobe to pick a sampling rate that spans the
/// whole video (a fixed fps would truncate long walkthroughs at the frame cap).
/// Frames come back as 512px-bounded JPEGs.
async fn extract_keyframes(
    video_bytes: &[u8],
    max_frames: usize,
) -> Result<Vec<Vec<u8>>, VolumeError> {
    let dir = tempfile::tempdir()
        .map_err(|e| VolumeError::Vision(format!("tempdir failed: {e}")))?;
    let input_path = dir.path().join("input.video");
    tokio::fs::write(&input_path, video_bytes)
        .await
        .map_err(|e| VolumeError::Vision(format!("video write failed: {e}")))?;

    // Probe duration (seconds). Fall back to 120 s if ffprobe is unavailable.
    let duration_s = match tokio::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(&input_path)
        .output()
        .await
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<f64>()
            .unwrap_or(120.0),
        _ => {
            tracing::warn!("ffprobe failed, assuming 120 s video");
            120.0
        }
    };

    // One frame every `interval` seconds so max_frames spans the full video.
    let interval = (duration_s / max_frames as f64).max(1.0);
    let fps_filter = format!("fps=1/{interval:.3},scale='min(512,iw)':-2");
    let out_pattern = dir.path().join("frame_%04d.jpg");

    let output = tokio::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i"])
        .arg(&input_path)
        .args(["-vf", &fps_filter, "-frames:v"])
        .arg(max_frames.to_string())
        .args(["-q:v", "4"])
        .arg(&out_pattern)
        .output()
        .await
        .map_err(|e| VolumeError::Vision(format!("ffmpeg spawn failed: {e}")))?;

    if !output.status.success() {
        return Err(VolumeError::Vision(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let mut frames = Vec::new();
    let mut entries = tokio::fs::read_dir(dir.path())
        .await
        .map_err(|e| VolumeError::Vision(format!("read_dir failed: {e}")))?;
    let mut paths = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| VolumeError::Vision(format!("read_dir failed: {e}")))?
    {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jpg") {
            paths.push(path);
        }
    }
    paths.sort();
    for path in paths {
        let data = tokio::fs::read(&path)
            .await
            .map_err(|e| VolumeError::Vision(format!("frame read failed: {e}")))?;
        frames.push(data);
    }

    if frames.is_empty() {
        return Err(VolumeError::Vision("ffmpeg produced no frames".into()));
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_is_embedded_and_complete() {
        assert!(RE_CATALOGUE.lines().count() >= 70);
        assert!(RE_CATALOGUE.contains("sofa | Sofa, Couch, Liege je Sitz"));
        assert!(RE_CATALOGUE.contains("NOT-MOVED"));
        assert!(RE_CATALOGUE.contains("BOX"));
    }

    #[test]
    fn parse_full_response() {
        let response = r#"{
            "movable_items": [
                {"key": "sofa", "german": "Sofa, Couch, Liege je Sitz", "count": 1, "volume_per_unit_m3": 2.0, "volume_m3": 2.0},
                {"key": "chair", "german": "Stuhl", "count": 4, "volume_per_unit_m3": 0.2, "volume_m3": 0.8}
            ],
            "boxes_estimate": 20,
            "not_moved": [{"item": "Einbauküche", "reason": "built-in"}],
            "total_movable_m3": 99.9
        }"#;
        let result = parse_vlm_response(response).unwrap();
        assert_eq!(result.detected_items.len(), 3);
        // Server-side total: 2.0 + 0.8 + 20*0.1 = 4.8 (model's 99.9 ignored)
        assert!((result.total_volume_m3 - 4.8).abs() < 1e-9);
        assert_eq!(result.detected_items[1].name, "4× Stuhl");
        assert_eq!(result.detected_items[2].name, "20× Umzugskarton");
        assert!(result.analysis_notes.as_deref().unwrap().contains("Einbauküche"));
    }

    #[test]
    fn parse_response_wrapped_in_markdown() {
        let response = "Here you go:\n```json\n{\"movable_items\":[{\"key\":\"bed\",\"count\":1,\"volume_m3\":1.5}],\"boxes_estimate\":0,\"not_moved\":[]}\n```";
        let result = parse_vlm_response(response).unwrap();
        assert_eq!(result.detected_items.len(), 1);
        assert!((result.total_volume_m3 - 1.5).abs() < 1e-9);
        // No german name in response — falls back to the key.
        assert_eq!(result.detected_items[0].name, "bed");
    }

    #[test]
    fn parse_line_volume_from_per_unit_when_missing() {
        let response = r#"{"movable_items":[{"key":"chair","count":3,"volume_per_unit_m3":0.2}],"boxes_estimate":0,"not_moved":[]}"#;
        let result = parse_vlm_response(response).unwrap();
        assert!((result.total_volume_m3 - 0.6).abs() < 1e-9);
    }

    #[test]
    fn parse_rejects_empty_inventory() {
        let response = r#"{"movable_items":[],"boxes_estimate":0,"not_moved":[]}"#;
        assert!(parse_vlm_response(response).is_err());
    }

    /// Live keyframe extraction against a synthetic ffmpeg-generated clip.
    /// Requires ffmpeg/ffprobe on PATH (present in docker/Dockerfile.backend).
    #[tokio::test]
    #[ignore = "requires ffmpeg on PATH; run with --ignored"]
    async fn live_extract_keyframes_from_synthetic_video() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.mp4");
        let status = tokio::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=duration=10:size=640x480:rate=10"])
            .arg(&clip)
            .status()
            .await
            .unwrap();
        assert!(status.success());
        let bytes = tokio::fs::read(&clip).await.unwrap();

        let frames = extract_keyframes(&bytes, 8).await.unwrap();
        assert!(!frames.is_empty() && frames.len() <= 8, "got {} frames", frames.len());
        // Frames decode as images bounded to 512px
        let img = image::load_from_memory(&frames[0]).unwrap();
        assert!(img.width() <= 512);
    }

    /// Full live run against Ollama Cloud with the 59-photo benchmark set.
    /// Takes ~14 min on minimax-m3. Requires AUST__LLM__OLLAMA__API_KEY in .env.
    #[tokio::test]
    #[ignore = "live Ollama Cloud call, ~14 min; run with --ignored"]
    async fn live_minimax_estimate_full_testset() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..");
        let env_text = std::fs::read_to_string(repo_root.join(".env")).unwrap();
        let api_key = env_text
            .lines()
            .find_map(|l| l.strip_prefix("AUST__LLM__OLLAMA__API_KEY="))
            .expect("AUST__LLM__OLLAMA__API_KEY not in .env")
            .trim()
            .to_string();

        let set_dir = repo_root.join(
            "services/vision/testsets/019dd584-faa1-72b3-8e0c-53cb097367b7/019dd585-06ac-7e90-b565-d0c598ac714e",
        );
        let mut paths: Vec<_> = std::fs::read_dir(&set_dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "jpg"))
            .collect();
        paths.sort();
        let images: Vec<(Vec<u8>, String)> = paths
            .iter()
            .map(|p| (std::fs::read(p).unwrap(), "image/jpeg".to_string()))
            .collect();
        assert!(!images.is_empty());
        eprintln!("running live VLM estimation on {} images ...", images.len());

        let estimator = VlmEstimator::new(
            "https://ollama.com".to_string(),
            Some(api_key),
            "minimax-m3".to_string(),
            1800,
        );
        let result = estimator.estimate_photos(&images).await.unwrap();

        eprintln!("TOTAL: {} m³", result.total_volume_m3);
        for item in &result.detected_items {
            eprintln!("  {} = {} m³", item.name, item.volume_m3);
        }
        if let Some(notes) = &result.analysis_notes {
            eprintln!("NOTES: {notes}");
        }
        // Gold standard ~37 m³; sanity-bound the live result generously.
        assert!(
            result.total_volume_m3 > 15.0 && result.total_volume_m3 < 70.0,
            "total {} m³ outside sanity bounds",
            result.total_volume_m3
        );
        assert!(result.detected_items.len() >= 10);
    }

    #[test]
    fn downscale_produces_bounded_jpeg() {
        // 800x600 solid-color PNG
        let img = image::DynamicImage::new_rgb8(800, 600);
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let jpeg = downscale_to_jpeg(&png, 512).unwrap();
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert!(decoded.width() <= 512 && decoded.height() <= 512);
    }
}
