use crate::OfferError;
use std::path::PathBuf;
use tokio::process::Command;

/// Convert xlsx bytes to PDF using LibreOffice headless.
///
/// Requires `libreoffice` to be installed and accessible in PATH.
/// On Ubuntu/Debian: `apt install libreoffice-calc`
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
