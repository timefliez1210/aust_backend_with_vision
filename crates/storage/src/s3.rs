use async_trait::async_trait;
use crate::{StorageError, StorageProvider};
use aws_sdk_s3::{
    config::{BehaviorVersion, Credentials, Region},
    primitives::ByteStream,
    Client,
};
use aust_core::config::StorageConfig;
use bytes::Bytes;
use tracing::instrument;

pub struct S3Storage {
    client: Client,
    bucket: String,
}

impl S3Storage {
    pub async fn new(config: &StorageConfig) -> Result<Self, StorageError> {
        let region = Region::new(config.region.clone().unwrap_or_else(|| "eu-central-1".to_string()));

        let mut s3_config_builder = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(region);

        if let Some(endpoint) = &config.endpoint {
            s3_config_builder = s3_config_builder
                .endpoint_url(endpoint)
                .force_path_style(true);
        }

        if let Ok(access_key) = std::env::var("AWS_ACCESS_KEY_ID") {
            if let Ok(secret_key) = std::env::var("AWS_SECRET_ACCESS_KEY") {
                let credentials = Credentials::new(access_key, secret_key, None, None, "env");
                s3_config_builder = s3_config_builder.credentials_provider(credentials);
            }
        }

        let s3_config = s3_config_builder.build();
        let client = Client::from_conf(s3_config);

        Ok(Self {
            client,
            bucket: config.bucket.clone(),
        })
    }
}

#[async_trait]
impl StorageProvider for S3Storage {
    #[instrument(skip(self, data))]
    async fn upload(&self, key: &str, data: Bytes, content_type: &str) -> Result<String, StorageError> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(data))
            .content_type(content_type)
            .send()
            .await
            .map_err(|e| StorageError::S3(e.to_string()))?;

        Ok(key.to_string())
    }

    #[instrument(skip(self))]
    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| StorageError::S3(e.to_string()))?;
        Ok(())
    }

    #[instrument(skip(self))]
    async fn download(&self, key: &str) -> Result<Bytes, StorageError> {
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| StorageError::S3(e.to_string()))?;

        let data = response
            .body
            .collect()
            .await
            .map_err(|e| StorageError::S3(e.to_string()))?;

        Ok(data.into_bytes())
    }

}
