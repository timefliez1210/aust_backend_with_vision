pub mod error;
pub mod traits;

mod local;
mod s3;

pub use error::StorageError;
pub use local::LocalStorage;
pub use s3::S3Storage;
pub use traits::StorageProvider;

use aust_core::config::StorageConfig;
use std::sync::Arc;

pub async fn create_provider(config: &StorageConfig) -> Result<Arc<dyn StorageProvider>, StorageError> {
    match config.provider.as_str() {
        "s3" => {
            let storage = S3Storage::new(config).await?;
            Ok(Arc::new(storage))
        }
        "local" => {
            let storage = LocalStorage::new(&config.bucket)?;
            Ok(Arc::new(storage))
        }
        provider => Err(StorageError::Configuration(format!(
            "Unknown storage provider: {provider}"
        ))),
    }
}
