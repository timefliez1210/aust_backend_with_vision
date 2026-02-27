use thiserror::Error;

/// All errors that can occur during offer generation.
///
/// **Caller**: `crates/api/src/routes/offers.rs`, `crates/email-agent`
/// **Why**: Provides a single error type that covers the three distinct failure
/// modes of the offer pipeline — pricing calculation, XLSX template manipulation,
/// PDF conversion via LibreOffice, and S3 upload.
#[derive(Debug, Error)]
pub enum OfferError {
    /// A pricing calculation failed (e.g. invalid input that produces NaN).
    #[error("Pricing error: {0}")]
    Pricing(String),

    /// The XLSX template ZIP could not be read, parsed, or modified.
    /// Wraps any error from ZIP I/O or XML string surgery.
    #[error("Template error: {0}")]
    Template(String),

    /// LibreOffice failed to convert the XLSX to PDF, or LibreOffice is not
    /// installed on the host.
    #[error("PDF generation error: {0}")]
    Pdf(String),

    /// The generated PDF could not be uploaded to S3/MinIO.
    #[error("Storage error: {0}")]
    Storage(String),
}
