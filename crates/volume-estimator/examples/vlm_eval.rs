//! Benchmark the production VLM estimator on the photo test sets.
//!
//! ```text
//! cargo run -p aust-volume-estimator --example vlm_eval -- <model> <runs> <out_dir> <set_dir>...
//! ```
//! `VLM_MAX_DIM` overrides the image size (default 512 px).
//! Reads `AUST__LLM__OLLAMA__API_KEY` from the environment or the repo `.env`.
//! Writes `<out_dir>/<set>_<run>.raw.txt` (model answer) and `.json`
//! (parsed `VisionAnalysisResult` + timing) per run.

use aust_volume_estimator::VlmEstimator;
use std::path::{Path, PathBuf};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [model, runs, out_dir, sets @ ..] = args.as_slice() else {
        eprintln!("usage: vlm_eval <model> <runs> <out_dir> <set_dir>...");
        std::process::exit(2);
    };
    let runs: usize = runs.parse().expect("runs must be a number");
    let out_dir = PathBuf::from(out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();

    let api_key = std::env::var("AUST__LLM__OLLAMA__API_KEY").ok().or_else(|| {
        let env = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env");
        std::fs::read_to_string(env).ok()?.lines().find_map(|l| {
            l.strip_prefix("AUST__LLM__OLLAMA__API_KEY=").map(|k| k.trim().to_string())
        })
    });
    let max_dim: u32 = std::env::var("VLM_MAX_DIM").ok().and_then(|v| v.parse().ok()).unwrap_or(512);
    let estimator = VlmEstimator::new("https://ollama.com".into(), api_key, model.clone(), 1800)
        .with_max_image_dim(max_dim);

    for set in sets {
        let set_dir = Path::new(set);
        let mut paths: Vec<PathBuf> = walk(set_dir)
            .into_iter()
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "heic"))
            })
            .collect();
        paths.sort();
        let images: Vec<(Vec<u8>, String)> = paths
            .iter()
            .map(|p| (std::fs::read(p).unwrap(), "image/jpeg".to_string()))
            .collect();
        let name = set_dir.file_name().unwrap().to_string_lossy().to_string();
        for run in 1..=runs {
            let t0 = std::time::Instant::now();
            let raw = estimator.estimate_photos_raw(&images).await;
            let secs = t0.elapsed().as_secs_f64();
            let stem = out_dir.join(format!("{name}_{run}"));
            match raw {
                Ok(raw) => {
                    std::fs::write(stem.with_extension("raw.txt"), &raw).unwrap();
                    let parsed = VlmEstimator::parse_response(&raw);
                    let total = parsed.as_ref().map(|r| r.total_volume_m3).ok();
                    let json = serde_json::json!({
                        "set": name, "run": run, "images": images.len(), "secs": secs,
                        "parse_error": parsed.as_ref().err().map(|e| e.to_string()),
                        "result": parsed.ok(),
                    });
                    std::fs::write(stem.with_extension("json"), serde_json::to_string_pretty(&json).unwrap()).unwrap();
                    println!("{name} run {run}: {} images, {secs:.0}s, total {total:?} m³", images.len());
                }
                Err(e) => println!("{name} run {run}: ERROR after {secs:.0}s: {e}"),
            }
        }
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() { out.extend(walk(&p)); } else { out.push(p); }
    }
    out
}
