use crate::OfferError;
use std::path::PathBuf;
use tokio::process::Command;

/// Convert XLSX bytes to a PDF using LibreOffice in headless mode.
///
/// **Caller**: `crates/api/src/routes/offers.rs` (after `generate_offer_xlsx`)
/// **Why**: LibreOffice faithfully renders the full XLSX layout — column widths,
/// merged cells, page breaks, and the print area — so the output PDF looks
/// identical to what a user would see when printing from Excel/Calc.
///
/// The function writes the XLSX to a temporary file, invokes LibreOffice
/// (`--headless --calc --convert-to pdf`), then reads the resulting `offer.pdf`
/// back into memory. The temp directory is cleaned up automatically on drop.
///
/// Requires `libreoffice` to be installed and accessible in `PATH`.
/// On Ubuntu/Debian: `apt install libreoffice-calc`
///
/// # Parameters
/// - `xlsx_bytes` — raw bytes of the generated XLSX file
///
/// # Returns
/// Raw PDF bytes ready to be uploaded to S3 or served directly.
///
/// # Errors
/// - `OfferError::Pdf` if the temp directory cannot be created
/// - `OfferError::Pdf` if `libreoffice` is not found or exits non-zero
/// - `OfferError::Pdf` if the output PDF file is missing after conversion
/// - `OfferError::Pdf` if reading the PDF bytes fails
pub async fn convert_xlsx_to_pdf(xlsx_bytes: &[u8]) -> Result<Vec<u8>, OfferError> {
    let tmp_dir = tempfile::tempdir()
        .map_err(|e| OfferError::Pdf(format!("Failed to create temp dir: {e}")))?;

    let xlsx_path = tmp_dir.path().join("offer.xlsx");
    tokio::fs::write(&xlsx_path, xlsx_bytes)
        .await
        .map_err(|e| OfferError::Pdf(format!("Failed to write temp xlsx: {e}")))?;

    let output = Command::new("libreoffice")
        .arg("--headless")
        .arg("--calc")
        .arg("--convert-to")
        .arg("pdf")
        .arg("--outdir")
        .arg(tmp_dir.path())
        .arg(&xlsx_path)
        .output()
        .await
        .map_err(|e| OfferError::Pdf(format!(
            "Failed to run libreoffice (is it installed?): {e}"
        )))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OfferError::Pdf(format!(
            "LibreOffice conversion failed: {stderr}"
        )));
    }

    let pdf_path: PathBuf = tmp_dir.path().join("offer.pdf");
    if !pdf_path.exists() {
        return Err(OfferError::Pdf(
            "LibreOffice did not produce a PDF file".into(),
        ));
    }

    let pdf_bytes = tokio::fs::read(&pdf_path)
        .await
        .map_err(|e| OfferError::Pdf(format!("Failed to read PDF output: {e}")))?;

    Ok(pdf_bytes)
}

/// The clearing-job variant of the KVA's second page (Entrümpelung and
/// Haushaltsauflösung share it), embedded at compile time — same approach as the
/// XLSX template, so no extra file has to reach the image.
///
/// Extracted from the 2025-246 offer; it carries only the boilerplate terms —
/// no customer name, address, or prices.
const CLEARING_PAGE_2: &[u8] = include_bytes!("../../../templates/entruempelung_kva_seite2.pdf");

/// Marker text that identifies the KVA's terms page. Used to confirm we are
/// about to replace the right page rather than trusting the index blindly —
/// if a long line-item list ever pushes the layout onto an extra page, the
/// terms are no longer page 2 and substituting by index would destroy the KVA.
const TERMS_PAGE_MARKER: &str = "Bei etwaigem Mehraufwand";

/// Replace page 2 of a generated KVA with the clearing-job terms page.
///
/// **Caller**: `offer_builder::run_offer_computation`, for the clearing service types
/// (`entruempelung`, `haushaltsaufloesung`) — see `uses_clearing_terms_page`.
/// **Why**: the XLSX template's page 2 lists Umzug-specific conditions (Kartons max.
/// 20 kg, Designermöbel, Tragewege) that do not apply to a clearing job. Umzüge and
/// every other service type keep the template's own page 2 untouched.
///
/// Page 1 (the priced KVA) and page 3 (the Arbeitszettel) are carried over
/// unchanged — only the middle page is swapped.
///
/// Uses `pdfseparate`/`pdfunite` from poppler-utils, already installed in the
/// backend image for the Telegram PDF pipeline.
///
/// # Errors
/// - `OfferError::Pdf` if the poppler tools are missing or fail
/// - `OfferError::Pdf` if the input has fewer than 2 pages, or page 2 is not the
///   terms page (checked via [`TERMS_PAGE_MARKER`]) — substituting blindly would
///   corrupt a customer-facing document, so this fails loudly instead
pub async fn substitute_clearing_page_2(pdf_bytes: &[u8]) -> Result<Vec<u8>, OfferError> {
    let tmp_dir = tempfile::tempdir()
        .map_err(|e| OfferError::Pdf(format!("Failed to create temp dir: {e}")))?;
    let dir = tmp_dir.path();

    let src = dir.join("offer.pdf");
    tokio::fs::write(&src, pdf_bytes)
        .await
        .map_err(|e| OfferError::Pdf(format!("Failed to write temp pdf: {e}")))?;

    // Split into single-page files: page-1.pdf, page-2.pdf, …
    let sep = Command::new("pdfseparate")
        .arg(&src)
        .arg(dir.join("page-%d.pdf"))
        .output()
        .await
        .map_err(|e| OfferError::Pdf(format!("Failed to run pdfseparate (poppler-utils?): {e}")))?;
    if !sep.status.success() {
        let stderr = String::from_utf8_lossy(&sep.stderr);
        return Err(OfferError::Pdf(format!("pdfseparate failed: {stderr}")));
    }

    let mut pages: Vec<PathBuf> = Vec::new();
    for n in 1.. {
        let p = dir.join(format!("page-{n}.pdf"));
        if !p.exists() {
            break;
        }
        pages.push(p);
    }

    if pages.len() < 2 {
        return Err(OfferError::Pdf(format!(
            "KVA has {} page(s); expected at least 2 to substitute the terms page",
            pages.len()
        )));
    }

    // Confirm page 2 really is the terms page before replacing it.
    let page_2_text = Command::new("pdftotext")
        .arg("-layout")
        .arg(&pages[1])
        .arg("-")
        .output()
        .await
        .map_err(|e| OfferError::Pdf(format!("Failed to run pdftotext: {e}")))?;
    let page_2_text = String::from_utf8_lossy(&page_2_text.stdout);
    if !page_2_text.contains(TERMS_PAGE_MARKER) {
        return Err(OfferError::Pdf(
            "Page 2 of the KVA is not the terms page — refusing to substitute".into(),
        ));
    }

    // Swap in the Entrümpelung page, then stitch everything back together.
    tokio::fs::write(&pages[1], CLEARING_PAGE_2)
        .await
        .map_err(|e| OfferError::Pdf(format!("Failed to write substitute page: {e}")))?;

    let out = dir.join("merged.pdf");
    let mut unite = Command::new("pdfunite");
    unite.args(&pages).arg(&out);
    let merged = unite
        .output()
        .await
        .map_err(|e| OfferError::Pdf(format!("Failed to run pdfunite (poppler-utils?): {e}")))?;
    if !merged.status.success() {
        let stderr = String::from_utf8_lossy(&merged.stderr);
        return Err(OfferError::Pdf(format!("pdfunite failed: {stderr}")));
    }

    tokio::fs::read(&out)
        .await
        .map_err(|e| OfferError::Pdf(format!("Failed to read merged PDF: {e}")))
}
