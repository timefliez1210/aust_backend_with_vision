use crate::StorageError;
use async_trait::async_trait;
use bytes::Bytes;

/// Pluggable object storage backend for offer PDFs and vision pipeline images.
///
/// Two implementations are provided:
/// - `S3Storage` — AWS S3 or MinIO (used in production and local dev with Docker).
/// - `LocalStorage` — stores files on the local filesystem (useful for testing).
///
/// Select the backend via `StorageConfig::provider` (`"s3"` or `"local"`).
///
/// **Caller**: Created once at startup by `create_provider(&storage_config)` and
/// shared as `Arc<dyn StorageProvider>` across offer generation and volume
/// estimation routes.
///
/// # Key naming conventions
/// - Offer PDFs: `offers/{offer_id}/Angebot_{offer_number}.pdf`
/// - Estimation images: `estimates/{quote_id}/{estimation_id}/{index}.jpg`
#[async_trait]
pub trait StorageProvider: Send + Sync {
    /// Upload bytes to object storage and return the storage key.
    ///
    /// **Caller**: `offer-generator` after producing the PDF bytes, and the
    /// vision pipeline routes after receiving uploaded images.
    ///
    /// # Parameters
    /// - `key` — Storage object key (path-like string, e.g. `"offers/abc123/Angebot.pdf"`).
    /// - `data` — Raw file bytes to store.
    /// - `content_type` — MIME type set on the stored object (e.g. `"application/pdf"`
    ///   or `"image/jpeg"`).
    ///
    /// # Returns
    /// The storage key that was written, allowing callers to persist it in the database.
    ///
    /// # Errors
    /// `StorageError` when the upload fails (network error, permission denied, etc.).
    async fn upload(&self, key: &str, data: Bytes, content_type: &str) -> Result<String, StorageError>;

    /// Download the object at `key` and return its raw bytes.
    ///
    /// **Caller**: `GET /api/v1/offers/{id}/pdf` to serve the PDF to the browser,
    /// and the Telegram approval flow to attach the PDF when emailing the customer.
    ///
    /// # Parameters
    /// - `key` — Storage object key previously returned by `upload`.
    ///
    /// # Errors
    /// `StorageError` when the object does not exist or the download fails.
    async fn download(&self, key: &str) -> Result<Bytes, StorageError>;

    /// Delete the object at `key`.
    ///
    /// **Caller**: Cleanup after offer rejection or after regenerating a PDF
    /// with different parameters.
    ///
    /// # Parameters
    /// - `key` — Storage object key to delete.
    ///
    /// # Errors
    /// `StorageError` when deletion fails. Callers should treat "not found" as
    /// a no-op rather than an error where possible.
    async fn delete(&self, key: &str) -> Result<(), StorageError>;
}
