# crates/storage — S3-Compatible Object Storage

Abstraction over file storage. Production: S3/MinIO. Development: local filesystem.

## StorageProvider Trait

```rust
#[async_trait]
pub trait StorageProvider: Send + Sync {
    async fn upload(&self, key: &str, data: Bytes, content_type: &str) -> Result<String, StorageError>;
    async fn download(&self, key: &str) -> Result<Bytes, StorageError>;
    async fn delete(&self, key: &str) -> Result<(), StorageError>;
}
```

`upload()` returns the key it wrote (`Ok(key)`), letting callers persist it in one
expression. There is no `exists()` — neither declared on the trait nor implemented
by either backend; use a try-download as a presence check.

## Implementations

- **S3Storage** — AWS S3 / MinIO via `aws-sdk-s3`
- **LocalStorage** — filesystem under configured bucket directory

## Key Convention

```
offers/{offer_id}/angebot.pdf                      — offer PDF (angebot.xlsx alongside it)
estimates/{inquiry_id}/{est_id}/{idx}.jpg          — estimation images
estimates/{inquiry_id}/{est_id}/depth/{idx}.{ext}  — depth maps (AR flow: .../ar/depth/{idx}.png)
estimates/{inquiry_id}/{est_id}/video.{ext}        — uploaded video for MASt3R reconstruction
estimates/{inquiry_id}/{est_id}/crops/{name}_{idx}.jpg — per-item crops
employees/{emp_id}/{doc_type}.{ext}                — employee documents (doc_type: arbeitsvertrag | mitarbeiterfragebogen)
feedback/{tmp_id}/{idx}.{ext}                      — feedback attachments
```

Both the offer PDF and its XLSX source are written to the same `offers/{offer_id}/`
prefix (`angebot.pdf` / `angebot.xlsx`) — not just the PDF.

## S3 Orphan Handling

Inquiry hard-delete (`routes/inquiries.rs`) collects all S3 keys — the offer PDF plus every estimation file returned by `collect_estimation_s3_keys()` — before the DB delete runs. Each failed deletion is logged individually with `warn!`; a follow-up `error!` log lists all failed keys together for manual cleanup. The DB row is deleted regardless of S3 failures (best-effort cleanup, not transactional).

## Configuration

Uses `StorageConfig` from core: `provider` (`"s3"`/`"local"`), `bucket`, `endpoint` (custom S3-compatible endpoint, e.g. MinIO; `None` uses AWS default), `region`. There is no `access_key`/`secret_key` field — `S3Storage` reads `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` directly from the process environment (falling back to the AWS SDK's default credential chain if unset).
## ⚠️ Connected Changes

| If you change... | ...also verify |
|---|---|
| S3 key convention | `offer_builder.rs` PDF keys, `submissions.rs` image keys, `admin.rs` employee document keys, `inquiries.rs` delete cleanup |
| `StorageProvider` trait signature | All callers: `offer_builder.rs`, `estimates.rs`, `submissions.rs`, `admin.rs` document upload/download |
