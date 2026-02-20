from __future__ import annotations

from pydantic import BaseModel, Field


class EstimateOptions(BaseModel):
    detection_threshold: float | None = None


class EstimateRequest(BaseModel):
    job_id: str
    s3_keys: list[str]
    options: EstimateOptions = Field(default_factory=EstimateOptions)


class ItemDimensions(BaseModel):
    length_m: float
    width_m: float
    height_m: float


class DetectedItem(BaseModel):
    name: str
    volume_m3: float
    dimensions: ItemDimensions
    confidence: float
    seen_in_images: list[int]
    category: str


class EstimateResponse(BaseModel):
    job_id: str
    status: str
    detected_items: list[DetectedItem]
    total_volume_m3: float
    confidence_score: float
    processing_time_ms: int


class HealthResponse(BaseModel):
    status: str


class ReadyResponse(BaseModel):
    status: str
    models_loaded: bool
    gpu_available: bool


# Internal intermediate types used across pipeline stages

class Detection(BaseModel):
    """A single detected object from Grounding DINO."""

    bbox: list[float] = Field(description="[x1, y1, x2, y2] in pixel coords")
    label: str
    confidence: float
    image_index: int


class SegmentedObject(BaseModel):
    """Detection enriched with a segmentation mask reference."""

    detection: Detection
    mask_index: int  # index into the masks array for this image


class DepthResult(BaseModel):
    """Depth estimation output for a single image."""

    image_index: int
    # The actual depth map is stored as a numpy array, not serialized here.
    # This model just tracks metadata.
    width: int
    height: int


class VolumeEstimate(BaseModel):
    """Volume estimate for a single object before deduplication."""

    label: str
    volume_m3: float
    dimensions: ItemDimensions
    confidence: float
    image_index: int
    category: str
    feature_vector: list[float] = Field(default_factory=list)


# Category mapping for common household / moving items
ITEM_CATEGORIES: dict[str, str] = {
    "sofa": "furniture",
    "couch": "furniture",
    "chair": "furniture",
    "table": "furniture",
    "desk": "furniture",
    "bed": "furniture",
    "mattress": "furniture",
    "wardrobe": "furniture",
    "dresser": "furniture",
    "shelf": "furniture",
    "bookshelf": "furniture",
    "cabinet": "furniture",
    "nightstand": "furniture",
    "tv": "electronics",
    "television": "electronics",
    "monitor": "electronics",
    "computer": "electronics",
    "laptop": "electronics",
    "speaker": "electronics",
    "lamp": "electronics",
    "refrigerator": "appliance",
    "fridge": "appliance",
    "washing machine": "appliance",
    "dryer": "appliance",
    "dishwasher": "appliance",
    "oven": "appliance",
    "microwave": "appliance",
    "stove": "appliance",
    "box": "boxes",
    "carton": "boxes",
    "package": "boxes",
    "suitcase": "luggage",
    "bag": "luggage",
    "bicycle": "sports",
    "bike": "sports",
    "piano": "instrument",
    "guitar": "instrument",
    "plant": "misc",
    "painting": "misc",
    "mirror": "misc",
    "rug": "misc",
    "carpet": "misc",
}


def classify_item(label: str) -> str:
    """Return a category string for a detected item label."""
    normalized = label.lower().strip()
    for key, category in ITEM_CATEGORIES.items():
        if key in normalized:
            return category
    return "misc"


# Reference dimensions (L, W, H in meters) for known household items.
# Used to calibrate monocular depth scale per image.
# These are typical/average sizes for the European market.
REFERENCE_DIMENSIONS: dict[str, tuple[float, float, float]] = {
    "sofa": (2.10, 0.90, 0.85),
    "couch": (2.10, 0.90, 0.85),
    "chair": (0.55, 0.55, 0.85),
    "table": (1.20, 0.75, 0.75),
    "desk": (1.20, 0.60, 0.75),
    "bed": (2.00, 1.60, 0.50),
    "mattress": (2.00, 1.40, 0.20),
    "wardrobe": (1.20, 0.60, 2.00),
    "dresser": (0.80, 0.48, 1.20),
    "shelf": (0.80, 0.30, 1.80),
    "bookshelf": (0.80, 0.28, 2.00),
    "cabinet": (0.80, 0.45, 1.00),
    "nightstand": (0.45, 0.40, 0.55),
    "tv": (1.23, 0.07, 0.72),
    "television": (1.23, 0.07, 0.72),
    "monitor": (0.60, 0.20, 0.45),
    "lamp": (0.30, 0.30, 0.50),
    "refrigerator": (0.60, 0.65, 1.80),
    "fridge": (0.60, 0.65, 1.80),
    "washing machine": (0.60, 0.60, 0.85),
    "dryer": (0.60, 0.60, 0.85),
    "dishwasher": (0.60, 0.60, 0.85),
    "oven": (0.60, 0.55, 0.60),
    "microwave": (0.50, 0.36, 0.30),
    "stove": (0.60, 0.60, 0.85),
    "box": (0.60, 0.40, 0.40),
    "carton": (0.60, 0.40, 0.40),
    "suitcase": (0.70, 0.45, 0.25),
    "bicycle": (1.70, 0.50, 1.00),
    "piano": (1.50, 0.60, 1.20),
    "rug": (2.00, 1.40, 0.02),
    "plant": (0.30, 0.30, 0.50),
    "mirror": (0.60, 0.03, 0.80),
    "painting": (0.60, 0.03, 0.50),
}


def get_reference_dims(label: str) -> tuple[float, float, float] | None:
    """Look up reference (L, W, H) for a label. Returns None if unknown."""
    normalized = label.lower().strip()
    for key, dims in REFERENCE_DIMENSIONS.items():
        if key in normalized:
            return dims
    return None
