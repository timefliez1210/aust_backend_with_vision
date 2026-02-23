from __future__ import annotations

import logging

from fastapi import APIRouter, File, Form, HTTPException, UploadFile, status

from app.models.schemas import EstimateResponse
from app.vision.model_loader import registry
from app.vision.video_pipeline import VideoPipeline

router = APIRouter(prefix="/estimate")
logger = logging.getLogger(__name__)

# Allowed video MIME types
_ALLOWED_TYPES = {
    "video/mp4",
    "video/quicktime",
    "video/webm",
    "video/x-matroska",
    "video/mpeg",
    "video/avi",
}

# Max upload size: 500 MB
_MAX_SIZE_BYTES = 500 * 1024 * 1024


@router.post("/video", response_model=EstimateResponse)
async def estimate_video(
    job_id: str = Form(default="test"),
    video: UploadFile = File(...),
    max_keyframes: int = Form(default=20),
    detection_threshold: float = Form(default=0.3),
) -> EstimateResponse:
    """Estimate moving volume from a room walkthrough video.

    Accepts a video file (mp4, mov, webm, mkv) and runs the full 3D
    reconstruction pipeline: keyframe extraction -> MASt3R point cloud ->
    DINO detection -> SAM 2 tracking -> OBB volume -> RE catalog lookup.

    Processing takes 2-10 minutes depending on video length and keyframe count.
    Falls back to per-keyframe Depth Anything V2 if 3D reconstruction fails.

    curl -X POST .../estimate/video \\
        -F "job_id=test-1" -F "video=@room_walkthrough.mp4"
    """
    if not registry.is_loaded:
        raise HTTPException(
            status_code=status.HTTP_503_SERVICE_UNAVAILABLE,
            detail="Models not loaded yet. Check /ready endpoint.",
        )

    # Validate content type
    content_type = video.content_type or ""
    if content_type not in _ALLOWED_TYPES and not content_type.startswith("video/"):
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=f"Invalid content type: {content_type}. Expected video file.",
        )

    # Read video data
    video_bytes = await video.read()
    if len(video_bytes) > _MAX_SIZE_BYTES:
        raise HTTPException(
            status_code=status.HTTP_413_REQUEST_ENTITY_TOO_LARGE,
            detail=f"Video too large: {len(video_bytes) / 1024 / 1024:.0f} MB "
                   f"(max {_MAX_SIZE_BYTES / 1024 / 1024:.0f} MB).",
        )

    if len(video_bytes) < 1000:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Video file too small to be valid.",
        )

    logger.info(
        "Video estimate request: job_id=%s, size=%.1f MB, type=%s",
        job_id, len(video_bytes) / 1024 / 1024, content_type,
    )

    pipeline = VideoPipeline(registry)
    try:
        result = pipeline.run(
            job_id=job_id,
            video_bytes=video_bytes,
            detection_threshold=detection_threshold,
            max_keyframes=max_keyframes,
        )
    except Exception as exc:
        logger.exception("Video pipeline failed for job %s", job_id)
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Video pipeline error: {exc}",
        ) from exc

    return result
