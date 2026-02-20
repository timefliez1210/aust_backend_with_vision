from __future__ import annotations

import asyncio
import logging
from contextlib import asynccontextmanager
from typing import AsyncIterator

from fastapi import FastAPI

from app.api.router import api_router
from app.config import settings
from app.vision.model_loader import registry

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
)
logger = logging.getLogger(__name__)


@asynccontextmanager
async def lifespan(app: FastAPI) -> AsyncIterator[None]:
    """Application lifespan: load models in background, serve health immediately."""
    logger.info("Starting model loading in background (device=%s) ...", settings.device)
    loop = asyncio.get_running_loop()
    loop.run_in_executor(None, _load_models)
    yield
    logger.info("Shutting down vision service.")


def _load_models() -> None:
    """Synchronous model loading, runs in a thread pool."""
    try:
        registry.load_all()
        logger.info("All models loaded successfully.")
    except Exception:
        logger.exception("Failed to load models. Service will run in degraded mode.")


app = FastAPI(
    title="AUST Vision Service",
    description="Vision-based volume estimation for moving company operations",
    version="0.1.0",
    lifespan=lifespan,
)

app.include_router(api_router)


if __name__ == "__main__":
    import uvicorn

    uvicorn.run(
        "app.main:app",
        host=settings.host,
        port=settings.port,
        log_level="info",
    )
