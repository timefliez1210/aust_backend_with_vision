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

    # Video pipeline - frame extraction
    video_dense_keyframes: int = 60       # Total keyframes for SAM 2 tracking
    video_recon_keyframes: int = 20       # Subset for MASt3R reconstruction
    video_max_keyframes: int = 60         # Kept for API compat (maps to dense)
    video_min_keyframes: int = 30         # Minimum dense keyframes
    video_blur_threshold: float = 50.0    # Lowered for phone video
    video_max_size_mb: int = 500

    # Detection
    detection_nms_threshold: float = 0.7  # NMS IoU for multi-prompt dedup
    cross_label_iou_threshold: float = 0.4  # Cross-label spatial merge

    # MASt3R reconstruction
    mast3r_min_conf: float = 1.5
    mast3r_batch_size: int = 1
    mast3r_alignment_iters: int = 300

    # Pause detection (for pause-aware detection frame selection)
    pause_min_duration_sec: float = 0.5      # Min pause length to detect
    pause_max_cluster_frames: int = 8        # Max frames sampled per pause segment


settings = Settings()
