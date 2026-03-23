# crates/storage — File Storage Abstraction

> External service map (S3/MinIO): [../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md#external-service-dependencies)

Abstract file storage with pluggable backends: S3-compatible (MinIO for dev, AWS for prod) or local filesystem.

## Key Files

- `src/lib.rs` - `StorageProvider` trait, S3/local implementations, factory

## StorageProvider Trait

```rust
#[async_trait]
pub trait StorageProvider: Send + Sync {
    async fn upload(&self, key: &str, data: &[u8], content_type: &str) -> Result<(), StorageError>;
    async fn download(&self, key: &str) -> Result<Vec<u8>, StorageError>;
}
```

## Implementations

- **S3Storage** — AWS S3 / MinIO via `aws-sdk-s3`
- **LocalStorage** — filesystem, stores under configured bucket directory

## Factory

```rust
let storage = create_provider(&storage_config).await?;
```

## Usage

S3 key convention for volume estimation images:
```
estimates/{inquiry_id}/{estimation_id}/{index}.jpg
```

## Configuration

Uses `StorageConfig` from core:
- `provider` — "s3" or "local"
- `bucket` — bucket name (default: "aust-uploads")
- `endpoint` — S3/MinIO URL
- `region`, `access_key`, `secret_key`
