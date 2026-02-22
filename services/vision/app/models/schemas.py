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
    # Furniture — seating
    "sofa": "furniture",
    "couch": "furniture",
    "armchair": "furniture",
    "chair": "furniture",
    "stool": "furniture",
    "bench": "furniture",
    "ottoman": "furniture",
    "recliner": "furniture",
    "bar stool": "furniture",
    # Furniture — tables
    "table": "furniture",
    "desk": "furniture",
    "dining table": "furniture",
    "coffee table": "furniture",
    "kitchen island": "furniture",
    # Furniture — beds
    "bed": "furniture",
    "mattress": "furniture",
    "crib": "furniture",
    "bunk bed": "furniture",
    # Furniture — storage
    "wardrobe": "furniture",
    "closet": "furniture",
    "dresser": "furniture",
    "chest of drawers": "furniture",
    "shelf": "furniture",
    "bookshelf": "furniture",
    "cabinet": "furniture",
    "cupboard": "furniture",
    "nightstand": "furniture",
    "shoe rack": "furniture",
    "coat rack": "furniture",
    # Electronics
    "tv": "electronics",
    "television": "electronics",
    "monitor": "electronics",
    "computer": "electronics",
    "laptop": "electronics",
    "printer": "electronics",
    "speaker": "electronics",
    "stereo": "electronics",
    "lamp": "electronics",
    "floor lamp": "electronics",
    "chandelier": "electronics",
    # Appliances
    "refrigerator": "appliance",
    "fridge": "appliance",
    "freezer": "appliance",
    "washing machine": "appliance",
    "dryer": "appliance",
    "dishwasher": "appliance",
    "oven": "appliance",
    "microwave": "appliance",
    "stove": "appliance",
    "vacuum cleaner": "appliance",
    "fan": "appliance",
    "heater": "appliance",
    # Boxes / containers
    "box": "boxes",
    "carton": "boxes",
    "moving box": "boxes",
    "package": "boxes",
    "basket": "boxes",
    "storage container": "boxes",
    # Luggage
    "suitcase": "luggage",
    "bag": "luggage",
    # Sports / fitness
    "bicycle": "sports",
    "bike": "sports",
    "treadmill": "sports",
    "exercise equipment": "sports",
    # Instruments
    "piano": "instrument",
    "keyboard": "instrument",
    "guitar": "instrument",
    # Misc
    "plant": "misc",
    "painting": "misc",
    "mirror": "misc",
    "rug": "misc",
    "carpet": "misc",
    "curtain": "misc",
    "ironing board": "misc",
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
    # Seating
    "sofa": (2.10, 0.90, 0.85),
    "couch": (2.10, 0.90, 0.85),
    "armchair": (0.90, 0.85, 0.85),
    "chair": (0.55, 0.55, 0.85),
    "stool": (0.40, 0.40, 0.50),
    "bench": (1.20, 0.40, 0.45),
    "ottoman": (0.70, 0.70, 0.45),
    "recliner": (0.90, 0.90, 1.00),
    "bar stool": (0.40, 0.40, 0.80),
    # Tables
    "table": (1.20, 0.75, 0.75),
    "desk": (1.20, 0.60, 0.75),
    "dining table": (1.60, 0.90, 0.75),
    "coffee table": (1.20, 0.60, 0.45),
    "kitchen island": (1.20, 0.80, 0.90),
    # Beds
    "bed": (2.00, 1.60, 0.50),
    "mattress": (2.00, 1.40, 0.20),
    "crib": (1.30, 0.70, 1.00),
    "bunk bed": (2.00, 1.00, 1.70),
    # Storage
    "wardrobe": (1.20, 0.60, 2.00),
    "closet": (1.20, 0.60, 2.00),
    "dresser": (0.80, 0.48, 1.20),
    "chest of drawers": (0.80, 0.48, 1.20),
    "shelf": (0.80, 0.30, 1.80),
    "bookshelf": (0.80, 0.28, 2.00),
    "cabinet": (0.80, 0.45, 1.00),
    "cupboard": (0.80, 0.45, 1.00),
    "nightstand": (0.45, 0.40, 0.55),
    "shoe rack": (0.60, 0.30, 0.80),
    "coat rack": (0.50, 0.50, 1.80),
    # Electronics
    "tv": (1.23, 0.07, 0.72),
    "television": (1.23, 0.07, 0.72),
    "monitor": (0.60, 0.20, 0.45),
    "computer": (0.45, 0.20, 0.45),
    "printer": (0.45, 0.40, 0.30),
    "speaker": (0.25, 0.25, 0.35),
    "stereo": (0.40, 0.30, 0.30),
    "lamp": (0.30, 0.30, 0.50),
    "floor lamp": (0.35, 0.35, 1.60),
    "chandelier": (0.60, 0.60, 0.50),
    # Appliances
    "refrigerator": (0.60, 0.65, 1.80),
    "fridge": (0.60, 0.65, 1.80),
    "freezer": (0.60, 0.65, 0.85),
    "washing machine": (0.60, 0.60, 0.85),
    "dryer": (0.60, 0.60, 0.85),
    "dishwasher": (0.60, 0.60, 0.85),
    "oven": (0.60, 0.55, 0.60),
    "microwave": (0.50, 0.36, 0.30),
    "stove": (0.60, 0.60, 0.85),
    "vacuum cleaner": (0.30, 0.30, 1.10),
    "fan": (0.40, 0.40, 0.50),
    "heater": (0.60, 0.20, 0.50),
    # Boxes / containers
    "box": (0.60, 0.40, 0.40),
    "carton": (0.60, 0.40, 0.40),
    "moving box": (0.60, 0.40, 0.40),
    "basket": (0.50, 0.40, 0.30),
    "storage container": (0.60, 0.40, 0.35),
    "suitcase": (0.70, 0.45, 0.25),
    "bag": (0.60, 0.35, 0.30),
    # Sports / fitness
    "bicycle": (1.70, 0.50, 1.00),
    "treadmill": (1.80, 0.80, 1.40),
    "exercise equipment": (1.50, 0.70, 1.20),
    # Instruments
    "piano": (1.50, 0.60, 1.20),
    "keyboard": (1.30, 0.35, 0.15),
    "guitar": (1.00, 0.40, 0.12),
    # Misc
    "rug": (2.00, 1.40, 0.02),
    "carpet": (2.00, 1.40, 0.02),
    "curtain": (2.00, 0.05, 2.50),
    "plant": (0.30, 0.30, 0.50),
    "mirror": (0.60, 0.03, 0.80),
    "painting": (0.60, 0.03, 0.50),
    "ironing board": (1.25, 0.40, 0.90),
}

# Maximum plausible volume (m³) per category.
# Detections exceeding these are likely errors and get filtered out.
MAX_VOLUME_BY_CATEGORY: dict[str, float] = {
    "furniture": 8.0,   # large wardrobe / bunk bed
    "electronics": 1.5,
    "appliance": 2.0,   # big fridge
    "boxes": 0.5,
    "luggage": 0.4,
    "sports": 4.0,      # treadmill
    "instrument": 3.0,  # grand piano
    "misc": 3.0,
}

# Packing multiplier per category.
# Converts raw object volume → truck-loading volume.
# Accounts for: irregular shapes, padding/wrapping, stacking gaps.
PACKING_MULTIPLIER: dict[str, float] = {
    "furniture": 1.2,
    "electronics": 1.4,   # needs wrapping / padding
    "appliance": 1.15,
    "boxes": 1.0,         # stacks efficiently
    "luggage": 1.0,
    "sports": 1.5,        # irregular shapes (bicycles etc.)
    "instrument": 1.6,    # fragile, needs padding
    "misc": 1.3,
}


def get_reference_dims(label: str) -> tuple[float, float, float] | None:
    """Look up reference (L, W, H) for a label. Returns None if unknown."""
    normalized = label.lower().strip()
    for key, dims in REFERENCE_DIMENSIONS.items():
        if key in normalized:
            return dims
    return None


def get_max_volume(category: str) -> float:
    """Return the maximum plausible volume for a category."""
    return MAX_VOLUME_BY_CATEGORY.get(category, 5.0)


def get_packing_multiplier(category: str) -> float:
    """Return the packing multiplier for a category."""
    return PACKING_MULTIPLIER.get(category, 1.3)
