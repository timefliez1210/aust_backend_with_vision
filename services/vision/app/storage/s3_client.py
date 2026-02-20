from __future__ import annotations

import io
import logging

import boto3
from botocore.config import Config as BotoConfig
from PIL import Image

from app.config import settings

logger = logging.getLogger(__name__)


class S3Client:
    """Thin wrapper around boto3 for downloading images from S3 / MinIO."""

    def __init__(self) -> None:
        self._client = boto3.client(
            "s3",
            endpoint_url=settings.s3_endpoint,
            aws_access_key_id=settings.s3_access_key,
            aws_secret_access_key=settings.s3_secret_key,
            region_name=settings.s3_region,
            config=BotoConfig(signature_version="s3v4"),
        )
        self._bucket = settings.s3_bucket

    def download_image(self, key: str) -> Image.Image:
        """Download an image from S3 and return it as a PIL Image."""
        logger.info("Downloading s3://%s/%s", self._bucket, key)
        response = self._client.get_object(Bucket=self._bucket, Key=key)
        data = response["Body"].read()
        image = Image.open(io.BytesIO(data)).convert("RGB")
        logger.info("Downloaded image %s: %dx%d", key, image.width, image.height)
        return image

    def download_images(self, keys: list[str]) -> list[Image.Image]:
        """Download multiple images, returning them in order."""
        images: list[Image.Image] = []
        for key in keys:
            images.append(self.download_image(key))
        return images
