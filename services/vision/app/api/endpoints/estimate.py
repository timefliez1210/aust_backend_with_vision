from __future__ import annotations

import logging

from fastapi import APIRouter, HTTPException, Request, status

from app.models.schemas import EstimateRequest, EstimateResponse
from app.storage.s3_client import S3Client
from app.vision.model_loader import registry
from app.vision.pipeline import VisionPipeline

router = APIRouter(prefix="/estimate")
logger = logging.getLogger(__name__)


@router.post("/images", response_model=EstimateResponse)
async def estimate_images(request: Request, body: EstimateRequest) -> EstimateResponse:
    """Run the full vision pipeline on uploaded images.

    Downloads images from S3, runs detection + segmentation + depth + volume,
    deduplicates cross-image items, and returns results.
    """
    if not registry.is_loaded:
        raise HTTPException(
            status_code=status.HTTP_503_SERVICE_UNAVAILABLE,
            detail="Models not loaded yet. Check /ready endpoint.",
        )

    if not body.s3_keys:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="s3_keys must not be empty.",
        )

    logger.info("Estimate request: job_id=%s, images=%d", body.job_id, len(body.s3_keys))

    # Download images from S3
    s3 = S3Client()
    try:
        images = s3.download_images(body.s3_keys)
    except Exception as exc:
        logger.error("Failed to download images for job %s: %s", body.job_id, exc)
        raise HTTPException(
            status_code=status.HTTP_422_UNPROCESSABLE_ENTITY,
            detail=f"Failed to download one or more images: {exc}",
        ) from exc

    # Run the pipeline
    pipeline = VisionPipeline(registry)
    try:
        result = pipeline.run(
            job_id=body.job_id,
            images=images,
            detection_threshold=body.options.detection_threshold,
        )
    except Exception as exc:
        logger.exception("Pipeline failed for job %s", body.job_id)
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Vision pipeline error: {exc}",
        ) from exc

    return result
