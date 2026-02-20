from __future__ import annotations

from fastapi import APIRouter

from app.api.endpoints import estimate, health

api_router = APIRouter()
api_router.include_router(health.router, tags=["health"])
api_router.include_router(estimate.router, tags=["estimate"])
