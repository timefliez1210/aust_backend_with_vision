//! Telegram media ingestion for the assistant: download a file by `file_id`,
//! rasterize PDFs to images, and base64-encode everything for the vision model.
//!
//! Photos are passed straight through. PDFs are rendered page-by-page with
//! `pdftoppm` (poppler-utils) so the vision-capable model sees them like images —
//! this handles both digitally-generated and scanned PDFs uniformly.

use base64::Engine;
use reqwest::Client;
use tracing::{debug, warn};

/// Hard cap on how many images one message forwards to the model (photos or
/// rasterized PDF pages). Keeps token cost per turn bounded.
pub const MAX_IMAGES: usize = 10;

/// Download a Telegram file by `file_id` and return its raw bytes.
///
/// Two-step Telegram flow: `getFile` resolves a temporary `file_path`, then the
/// bytes are fetched from the file CDN endpoint.
async fn download_file(
    client: &Client,
    bot_token: &str,
    file_id: &str,
) -> Result<Vec<u8>, String> {
    let get_file_url = format!("https://api.telegram.org/bot{bot_token}/getFile");
    let resp = client
        .get(&get_file_url)
        .query(&[("file_id", file_id)])
        .send()
        .await
        .map_err(|e| format!("getFile request failed: {e}"))?;

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("getFile parse failed: {e}"))?;

    let file_path = json["result"]["file_path"]
        .as_str()
        .ok_or_else(|| format!("getFile returned no file_path: {json}"))?;

    let dl_url = format!("https://api.telegram.org/file/bot{bot_token}/{file_path}");
    let bytes = client
        .get(&dl_url)
        .send()
        .await
        .map_err(|e| format!("file download failed: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("file body read failed: {e}"))?;

    Ok(bytes.to_vec())
}

/// Rasterize a PDF to PNG pages (at most `max_pages`) via `pdftoppm`.
///
/// Runs in a blocking thread because it shells out and touches the filesystem.
fn pdf_to_pngs(pdf_bytes: Vec<u8>, max_pages: usize) -> Result<Vec<Vec<u8>>, String> {
    use std::io::Write;
    use std::process::Command;

    let dir = std::env::temp_dir().join(format!("aust_pdf_{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("tempdir: {e}"))?;
    let pdf_path = dir.join("in.pdf");
    let mut f = std::fs::File::create(&pdf_path).map_err(|e| format!("write pdf: {e}"))?;
    f.write_all(&pdf_bytes).map_err(|e| format!("write pdf: {e}"))?;
    drop(f);

    let prefix = dir.join("page");
    // -r 150 DPI is enough for the model to read text; -l limits last page.
    let output = Command::new("pdftoppm")
        .arg("-png")
        .arg("-r")
        .arg("150")
        .arg("-l")
        .arg(max_pages.to_string())
        .arg(&pdf_path)
        .arg(&prefix)
        .output();

    let result = (|| {
        let output = output.map_err(|e| {
            format!("pdftoppm not available ({e}) — install poppler-utils")
        })?;
        if !output.status.success() {
            return Err(format!(
                "pdftoppm failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        // Collect generated PNGs in name order (page-1.png, page-2.png, …).
        let mut pages: Vec<(String, Vec<u8>)> = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("read tempdir: {e}"))? {
            let entry = entry.map_err(|e| format!("dir entry: {e}"))?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("png") {
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                let bytes = std::fs::read(&path).map_err(|e| format!("read png: {e}"))?;
                pages.push((name, bytes));
            }
        }
        pages.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(pages.into_iter().take(max_pages).map(|(_, b)| b).collect::<Vec<_>>())
    })();

    // Best-effort cleanup regardless of outcome.
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Download a Telegram photo or document and return base64-encoded images ready
/// for the model. PDFs are rasterized; other documents that aren't images are
/// rejected with a user-facing message.
///
/// Returns `Ok(images)` (1..=MAX_IMAGES) or `Err(message_de)` describing why the
/// media could not be turned into images (shown to Alex).
pub async fn prepare_images(
    client: &Client,
    bot_token: &str,
    file_id: &str,
    kind: &str,
    mime_type: Option<&str>,
) -> Result<Vec<String>, String> {
    let bytes = download_file(client, bot_token, file_id)
        .await
        .map_err(|e| {
            warn!("media download failed: {e}");
            "Ich konnte die Datei nicht von Telegram laden. Bitte versuche es erneut.".to_string()
        })?;

    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);

    // Photos are always images.
    if kind == "photo" {
        return Ok(vec![b64(&bytes)]);
    }

    // Documents: branch on MIME.
    let mime = mime_type.unwrap_or("").to_lowercase();
    if mime == "application/pdf" || (mime.is_empty() && bytes.starts_with(b"%PDF")) {
        debug!("rasterizing PDF ({} bytes)", bytes.len());
        let pages = tokio::task::spawn_blocking(move || pdf_to_pngs(bytes, MAX_IMAGES))
            .await
            .map_err(|e| format!("PDF-Verarbeitung abgebrochen: {e}"))?
            .map_err(|e| {
                warn!("pdf rasterize failed: {e}");
                "Ich konnte das PDF nicht in Bilder umwandeln (pdftoppm-Fehler).".to_string()
            })?;
        if pages.is_empty() {
            return Err("Das PDF enthielt keine darstellbaren Seiten.".to_string());
        }
        return Ok(pages.iter().map(|p| b64(p)).collect());
    }

    if mime.starts_with("image/") {
        return Ok(vec![b64(&bytes)]);
    }

    Err(format!(
        "Diesen Dateityp ({}) kann ich noch nicht lesen — ich unterstütze Bilder und PDFs.",
        if mime.is_empty() { "unbekannt" } else { &mime }
    ))
}
