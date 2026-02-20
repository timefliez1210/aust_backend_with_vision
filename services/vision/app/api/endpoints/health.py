from __future__ import annotations

from fastapi import APIRouter, Response, status

from app.models.schemas import HealthResponse, ReadyResponse
from app.vision.model_loader import registry

router = APIRouter()


@router.get("/health", response_model=HealthResponse)
async def health() -> HealthResponse:
    """Liveness probe. Always returns 200."""
    return HealthResponse(status="ok")


@router.get("/ready", response_model=ReadyResponse)
async def ready(response: Response) -> ReadyResponse:
    """Readiness probe. Returns 503 if models are not loaded."""
    if not registry.is_loaded:
        response.status_code = status.HTTP_503_SERVICE_UNAVAILABLE
        return ReadyResponse(
            status="not_ready",
            models_loaded=False,
            gpu_available=registry.gpu_available,
        )

    return ReadyResponse(
        status="ready",
        models_loaded=True,
        gpu_available=registry.gpu_available,
    )
