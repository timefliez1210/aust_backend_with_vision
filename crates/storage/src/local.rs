use async_trait::async_trait;
use crate::{StorageError, StorageProvider};
use bytes::Bytes;
use std::path::PathBuf;
use tokio::fs;
use tracing::instrument;

pub struct LocalStorage {
    base_path: PathBuf,
}

impl LocalStorage {
    pub fn new(base_path: &str) -> Result<Self, StorageError> {
        let path = PathBuf::from(base_path);
        std::fs::create_dir_all(&path)?;

        Ok(Self { base_path: path })
    }

    fn get_full_path(&self, key: &str) -> PathBuf {
        self.base_path.join(key)
    }
}

#[async_trait]
impl StorageProvider for LocalStorage {
    #[instrument(skip(self, data))]
    async fn upload(&self, key: &str, data: Bytes, _content_type: &str) -> Result<String, StorageError> {
        let path = self.get_full_path(key);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&path, &data).await?;

        Ok(key.to_string())
    }

    #[instrument(skip(self))]
    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let path = self.get_full_path(key);
        if path.exists() {
            fs::remove_file(&path).await?;
        }
        Ok(())
    }

    #[instrument(skip(self))]
    async fn download(&self, key: &str) -> Result<Bytes, StorageError> {
        let path = self.get_full_path(key);

        if !path.exists() {
            return Err(StorageError::NotFound(key.to_string()));
        }

        let data = fs::read(&path).await?;
        Ok(Bytes::from(data))
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn download_missing_key_returns_not_found() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let storage = LocalStorage::new(dir.path().to_str().unwrap()).unwrap();

        let err = storage.download("nonexistent/file.pdf").await.unwrap_err();
        assert!(
            matches!(err, StorageError::NotFound(_)),
            "expected NotFound, got {err:?}"
        );
    }

    #[tokio::test]
    async fn download_existing_key_returns_bytes() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let storage = LocalStorage::new(dir.path().to_str().unwrap()).unwrap();

        let data = Bytes::from_static(b"hello pdf");
        storage.upload("offers/test.pdf", data.clone(), "application/pdf").await.unwrap();

        let result = storage.download("offers/test.pdf").await.unwrap();
        assert_eq!(result, data);
    }
}
