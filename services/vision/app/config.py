from __future__ import annotations

from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(env_prefix="VISION_", env_file=".env", extra="ignore")

    # S3 / MinIO
    s3_endpoint: str = "http://minio:9000"
    s3_bucket: str = "aust-uploads"
    s3_access_key: str = "minioadmin"
    s3_secret_key: str = "minioadmin"
    s3_region: str = "us-east-1"

    # Device
    device: str = "cuda"

    # Model weights
    weights_dir: str = "/app/weights"

    # Detection
    detection_threshold: float = 0.3

    # Server
    host: str = "0.0.0.0"
    port: int = 8090

    # Deduplication
    dedup_similarity_threshold: float = 0.85

    # Depth estimation
    depth_scale: float = 1.0


settings = Settings()
