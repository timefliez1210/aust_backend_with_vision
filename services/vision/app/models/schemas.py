from __future__ import annotations

import math

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
    german_name: str | None = None
    re_value: float | None = None
    units: int | None = None
    volume_source: str = "geometric"
    bbox: list[float] | None = None
    bbox_image_index: int | None = None
    crop_base64: str | None = None


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
    volume_source: str = "geometric"  # "re" or "geometric"
    re_value: float | None = None
    german_name: str | None = None
    units: int | None = None
    bbox: list[float] | None = None          # [x1, y1, x2, y2] pixels
    crop_base64: str | None = None           # JPEG thumbnail, max 300px wide


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
    # Children
    "stroller": "misc",
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


# ===== RE (Raumeinheit) Volume Catalog =====
#
# Source: Umzugsgutliste Alltransport 24 Umzüge Hannover
# 1 RE = 0.1 m³ (German moving industry standard)
#
# This is the single source of truth for volume estimation used by
# both the VolumeCalculator web form and Alex for manual quotes.
#
# Entry types:
#   Fixed:       {"re": N}
#     Volume = re × 0.1 m³. Detection alone is enough.
#
#   SizeVariant: {"variants": [(max_dim_m, re), ...], "dim": "largest"|"second"|"height"}
#     Measure the specified dimension, pick first variant where measured ≤ threshold.
#     Last entry should use 99.0 as catch-all.
#
#   PerUnit:     {"re_per_unit": N, "unit": "seat"|"meter", "unit_size_m": W}
#     Measure width, count units = ceil(width / unit_size_m), volume = re × units × 0.1 m³.
#     For seats: units = round(width / unit_size_m). For meters: units = ceil(width).
#
# All entries include "german" key with the Umzugsgutliste item name.

RE_M3 = 0.1  # 1 RE = 0.1 m³

_INF = 99.0  # catch-all threshold for size variants

RE_CATALOG: dict[str, dict] = {
    # ── SEATING ──
    "sofa": {
        "re_per_unit": 4, "unit": "seat", "unit_size_m": 0.65,
        "default_units": 3,
        "german": "Sofa, Couch, Liege je Sitz",
    },
    "couch": {
        "re_per_unit": 4, "unit": "seat", "unit_size_m": 0.65,
        "default_units": 3,
        "german": "Sofa, Couch, Liege je Sitz",
    },
    "armchair": {"re": 8, "german": "Sessel mit Armlehnen"},
    "chair": {"re": 2, "german": "Stuhl"},
    "stool": {"re": 2, "german": "Stuhl, Hocker"},
    "bench": {
        "re_per_unit": 2, "unit": "seat", "unit_size_m": 0.55,
        "default_units": 3,
        "german": "Eckbank je Sitz",
    },
    "ottoman": {"re": 4, "german": "Ottoman"},
    "recliner": {"re": 8, "german": "Sessel mit Armlehnen"},
    "bar stool": {"re": 3, "german": "Stuhl mit Armlehnen"},

    # ── TABLES ──
    "table": {
        "variants": [(0.6, 4), (1.0, 5), (1.2, 6), (_INF, 8)],
        "dim": "largest",
        "german": "Tisch",
    },
    "desk": {
        "variants": [(1.6, 8), (_INF, 12)],
        "dim": "largest",
        "german": "Schreibtisch",
    },
    "dining table": {"re": 8, "german": "Tisch über 1,2 m"},
    "coffee table": {"re": 5, "german": "Tisch bis 1,0 m"},
    "kitchen island": {"re": 12, "german": "Winkelkombination"},

    # ── BEDS ──
    "bed": {
        # Single ≤110cm, Queen/French ≤150cm, Double >150cm
        # Use second-largest dimension (width), since length is always ~200cm
        "variants": [(1.1, 10), (1.5, 15), (_INF, 20)],
        "dim": "second",
        "german": "Bett",
    },
    "mattress": {
        "variants": [(1.1, 10), (1.5, 15), (_INF, 20)],
        "dim": "second",
        "german": "Bett (Matratze)",
    },
    "crib": {"re": 5, "german": "Kinderbett komplett"},
    "bunk bed": {"re": 16, "german": "Etagenbett komplett"},

    # ── STORAGE ──
    "wardrobe": {
        "re_per_unit": 8, "unit": "meter", "unit_size_m": 1.0,
        "default_units": 1,
        "german": "Schrank zerlegbar je angef. m",
    },
    "closet": {
        "re_per_unit": 8, "unit": "meter", "unit_size_m": 1.0,
        "default_units": 1,
        "german": "Schrank zerlegbar je angef. m",
    },
    "dresser": {"re": 7, "german": "Kommode"},
    "chest of drawers": {"re": 7, "german": "Kommode"},
    "shelf": {
        "re_per_unit": 4, "unit": "meter", "unit_size_m": 1.0,
        "default_units": 1,
        "german": "Regal zerlegbar je angef. m",
    },
    "bookshelf": {
        "re_per_unit": 4, "unit": "meter", "unit_size_m": 1.0,
        "default_units": 1,
        "german": "Bücherregal zerlegbar je angef. m",
    },
    "cabinet": {"re": 7, "german": "Kommode"},
    "cupboard": {
        "re_per_unit": 8, "unit": "meter", "unit_size_m": 1.0,
        "default_units": 1,
        "german": "Wohnz.-Schrank zerlegb. je angef. m",
    },
    "nightstand": {"re": 2, "german": "Nachttisch"},
    "shoe rack": {"re": 4, "german": "Schuhschrank"},
    "coat rack": {"re": 2, "german": "Kleiderablage"},

    # ── ELECTRONICS ──
    "tv": {"re": 3, "german": "Fernseher"},
    "television": {"re": 3, "german": "Fernseher"},
    "monitor": {"re": 3, "german": "Fernseher"},
    "computer": {"re": 5, "german": "Computer: PC / EDV-Anlage"},
    "laptop": {"re": 1, "german": "Computer (Laptop)"},
    "printer": {"re": 5, "german": "Tischkopierer"},
    "speaker": {"re": 4, "german": "Stereoanlage"},
    "stereo": {"re": 4, "german": "Stereoanlage"},
    "lamp": {"re": 2, "german": "Deckenlampe"},
    "floor lamp": {"re": 2, "german": "Stehlampe"},
    "chandelier": {"re": 5, "german": "Lüster"},

    # ── APPLIANCES ──
    "refrigerator": {
        # Small (under-counter) ≤120cm height, large (full-size) >120cm
        "variants": [(1.2, 5), (_INF, 10)],
        "dim": "height",
        "german": "Kühlschrank",
    },
    "fridge": {
        "variants": [(1.2, 5), (_INF, 10)],
        "dim": "height",
        "german": "Kühlschrank",
    },
    "freezer": {"re": 5, "german": "Kühlschrank / Truhe bis 120 l"},
    "washing machine": {"re": 5, "german": "Waschmaschine / Trockner"},
    "dryer": {"re": 5, "german": "Waschmaschine / Trockner"},
    "dishwasher": {"re": 5, "german": "Geschirrspülmaschine"},
    "oven": {"re": 5, "german": "Herd"},
    "microwave": {"re": 2, "german": "Mikrowelle"},
    "stove": {"re": 5, "german": "Herd"},
    "vacuum cleaner": {"re": 1, "german": "Staubsauger"},
    "fan": {"re": 2, "german": "Ventilator"},
    "heater": {"re": 2, "german": "Heizgerät"},

    # ── BOXES / CONTAINERS ──
    "box": {"re": 1, "german": "Umzugskarton bis 80 l"},
    "carton": {"re": 1, "german": "Umzugskarton bis 80 l"},
    "moving box": {"re": 1, "german": "Umzugskarton bis 80 l"},
    "basket": {"re": 1, "german": "Korb"},
    "storage container": {"re": 1.5, "german": "Umzugskarton über 80 l"},

    # ── CHILDREN ──
    "stroller": {"re": 3, "german": "Kinderwagen"},

    # ── LUGGAGE ──
    "suitcase": {"re": 1, "german": "Koffer"},
    "bag": {"re": 1, "german": "Tasche"},

    # ── SPORTS / FITNESS ──
    "bicycle": {"re": 5, "german": "Fahrrad / Moped"},
    "bike": {"re": 5, "german": "Fahrrad / Moped"},
    "treadmill": {"re": 10, "german": "Laufband"},
    "exercise equipment": {"re": 5, "german": "Sportgerät"},

    # ── INSTRUMENTS ──
    "piano": {"re": 15, "german": "Klavier"},
    "keyboard": {"re": 4, "german": "Keyboard"},
    "guitar": {"re": 2, "german": "Gitarre"},

    # ── MISC ──
    "plant": {"re": 1, "german": "Blumenkübel / Kasten"},
    "painting": {
        "variants": [(0.8, 1), (_INF, 2)],
        "dim": "largest",
        "german": "Bilder",
    },
    "mirror": {"re": 1, "german": "Spiegel"},
    "rug": {"re": 3, "german": "Teppich"},
    "carpet": {"re": 3, "german": "Teppich"},
    "curtain": {"re": 1, "german": "Vorhang"},
    "ironing board": {"re": 1, "german": "Bügelbrett"},
}


def _find_re_entry(label: str) -> dict | None:
    """Find the RE catalog entry for a detection label."""
    normalized = label.lower().strip()
    # Exact match first
    if normalized in RE_CATALOG:
        return RE_CATALOG[normalized]
    # Substring match (e.g., "large sofa" → "sofa")
    for key in RE_CATALOG:
        if key in normalized:
            return RE_CATALOG[key]
    return None


def lookup_re_volume(
    label: str,
    largest_dim_m: float | None = None,
    second_dim_m: float | None = None,
    height_m: float | None = None,
) -> tuple[float, float, int, str] | None:
    """Look up volume for a detected item using the RE catalog.

    Uses measured dimensions for size disambiguation and unit counting.

    Args:
        label: Detection label (English, from DINO).
        largest_dim_m: Largest measured dimension (usually width/length).
        second_dim_m: Second-largest measured dimension.
        height_m: Measured height (vertical extent).

    Returns:
        (volume_m3, re_total, units, german_name) or None if not in catalog.
    """
    entry = _find_re_entry(label)
    if entry is None:
        return None

    german = entry["german"]

    # Type 1: Fixed RE
    if "re" in entry:
        re_val = entry["re"]
        return (re_val * RE_M3, re_val, 1, german)

    # Type 2: Size variants
    if "variants" in entry:
        dim_key = entry.get("dim", "largest")
        if dim_key == "largest":
            measured = largest_dim_m
        elif dim_key == "second":
            measured = second_dim_m
        elif dim_key == "height":
            measured = height_m
        else:
            measured = largest_dim_m

        variants = entry["variants"]
        # Default to last (largest) variant
        re_val = variants[-1][1]

        if measured is not None:
            for max_dim, re in variants:
                if measured <= max_dim:
                    re_val = re
                    break

        return (re_val * RE_M3, re_val, 1, german)

    # Type 3: Per-unit
    if "re_per_unit" in entry:
        re_per_unit = entry["re_per_unit"]
        unit_type = entry["unit"]
        unit_size = entry["unit_size_m"]
        default_units = entry.get("default_units", 1)

        if largest_dim_m is not None and unit_size > 0:
            if unit_type == "seat":
                units = max(1, round(largest_dim_m / unit_size))
            else:  # "meter"
                units = max(1, math.ceil(largest_dim_m))
        else:
            units = default_units

        total_re = re_per_unit * units
        return (total_re * RE_M3, total_re, units, german)

    return None
