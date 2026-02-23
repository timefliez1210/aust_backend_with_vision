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

    # Video pipeline
    video_max_keyframes: int = 20
    video_min_keyframes: int = 10
    video_blur_threshold: float = 100.0
    video_max_size_mb: int = 500

    # MASt3R reconstruction
    mast3r_min_conf: float = 1.5
    mast3r_batch_size: int = 1
    mast3r_alignment_iters: int = 300


settings = Settings()
