# crates/storage — S3-Compatible Object Storage

Abstraction over file storage. Production: S3/MinIO. Development: local filesystem.

## StorageProvider Trait

```rust
#[async_trait]
pub trait StorageProvider: Send + Sync {
    async fn upload(&self, key: &str, data: Bytes, content_type: &str) -> Result<(), StorageError>;
    async fn download(&self, key: &str) -> Result<Bytes, StorageError>;
    async fn delete(&self, key: &str) -> Result<(), StorageError>;
    async fn exists(&self, key: &str) -> Result<bool, StorageError>;
}
```

## Implementations

- **S3Storage** — AWS S3 / MinIO via `aws-sdk-s3`
- **LocalStorage** — filesystem under configured bucket directory

## Key Convention

```
offers/{uuid}/angebot.pdf          — offer PDFs
estimates/{inquiry_id}/{est_id}/images/{idx}.jpg  — estimation images
estimates/{inquiry_id}/{est_id}/depth/{idx}.bin   — depth maps
employees/{emp_id}/arbeitsvertrag.pdf              — employee documents
feedback/{report_id}/{idx}.{ext}                  — feedback attachments
```

## S3 Orphan Handling (L2)

Inquiry hard-delete collects all S3 keys (offer PDFs + estimation images) before DB delete. Individual failures are logged with `warn!`. A summary `error!` log lists all failed keys for manual cleanup.

## Configuration

Uses `StorageConfig` from core: `provider` ("s3"/"local"), `bucket`, `endpoint`, `region`, `access_key`, `secret_key`.
## ⚠️ Connected Changes

| If you change... | ...also verify |
|---|---|
| S3 key convention | `offer_builder.rs` PDF keys, `submissions.rs` image keys, `admin.rs` employee document keys, `inquiries.rs` delete cleanup |
| `StorageProvider` trait signature | All callers: `offer_builder.rs`, `estimates.rs`, `submissions.rs`, `admin.rs` document upload/download |
