use crate::StorageError;
use async_trait::async_trait;
use bytes::Bytes;

#[async_trait]
pub trait StorageProvider: Send + Sync {
    async fn upload(&self, key: &str, data: Bytes, content_type: &str) -> Result<String, StorageError>;

    async fn download(&self, key: &str) -> Result<Bytes, StorageError>;

    async fn delete(&self, key: &str) -> Result<(), StorageError>;

    async fn exists(&self, key: &str) -> Result<bool, StorageError>;

    async fn get_presigned_url(&self, key: &str, expires_in_secs: u64) -> Result<String, StorageError>;
}
