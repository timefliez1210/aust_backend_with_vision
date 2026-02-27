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
