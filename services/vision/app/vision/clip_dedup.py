"""CLIP-based cross-image deduplication.

After within-image dedup, the same sofa photographed from 10 angles becomes
10 separate items and 10× the volume.  CLIP encodes every crop thumbnail into
a 768-dim visual embedding and clusters items whose embeddings are
cosine-similar above a threshold — but *only* when they appear in different
source images (same-image pairs already went through within-image dedup).

This runs BEFORE the Qwen2-VL step so that Qwen sees ≤24 items instead of
250, making its single-pass dedup actually effective.

GPU budget: CLIP ViT-L/14 in fp16 ≈ 0.7 GB, loaded on-demand and freed
immediately after the dedup pass — no effect on the DINO/SAM/DA budget.
"""
from __future__ import annotations

import base64
import io
import logging
from pathlib import Path
from typing import TYPE_CHECKING

import numpy as np
import torch

if TYPE_CHECKING:
    from app.vision.model_loader import ModelRegistry

from app.models.schemas import DetectedItem

logger = logging.getLogger(__name__)

CLIP_MODEL_ID = "openai/clip-vit-large-patch14"

# Cosine similarity above which two items (from different images) are merged.
# 0.85 empirically separates same-object-different-angle from same-type-different-object.
_SIMILARITY_THRESHOLD = 0.85


def clip_dedup(
    items: list[DetectedItem],
    registry: "ModelRegistry",
) -> list[DetectedItem]:
    """Reduce cross-image duplicates using CLIP visual embeddings.

    Called by: VisionPipeline.run() — Stage 5.5, between within-image dedup
               and VLM dedup (Stage 6).
    Purpose: 52 photos → ~250 items is mostly cross-image duplication (same
             sofa from 10 angles = 10 entries). CLIP clusters these into single
             items so the Qwen VLM step sees the true item count (~30-60)
             rather than the inflated raw count.

    Algorithm:
      1. Encode all crop thumbnails as CLIP image embeddings (batch, GPU).
      2. Build an N×N cosine similarity matrix.
      3. Greedy clustering: assign each item to the first existing cluster
         whose representative has similarity > threshold, but only if the two
         items come from different source images.
      4. Per cluster: keep the highest-confidence detection, merge seen_in_images.

    Args:
        items:    DetectedItem list after within-image dedup.
        registry: ModelRegistry (provides device; CLIP is loaded/unloaded here).

    Returns:
        Deduplicated DetectedItem list.  Falls back to input unchanged on any error.
    """
    items_with_crop = [(i, item) for i, item in enumerate(items) if item.crop_base64]
    if len(items_with_crop) < 2:
        logger.info("CLIP dedup: fewer than 2 items with crops — skipping")
        return items

    try:
        return _run(items, items_with_crop, registry)
    except Exception:
        logger.exception("CLIP dedup raised — returning items unchanged")
        return items


# ---------------------------------------------------------------------------
# Internals
# ---------------------------------------------------------------------------

def _run(
    all_items: list[DetectedItem],
    items_with_crop: list[tuple[int, DetectedItem]],
    registry: "ModelRegistry",
) -> list[DetectedItem]:
    from PIL import Image
    from transformers import CLIPModel, CLIPProcessor
    from app.config import settings

    cache_dir = str(Path(settings.weights_dir) / "huggingface")
    device = registry.device

    logger.info("CLIP dedup: loading %s …", CLIP_MODEL_ID)
    processor = CLIPProcessor.from_pretrained(CLIP_MODEL_ID, cache_dir=cache_dir)
    model = CLIPModel.from_pretrained(
        CLIP_MODEL_ID, torch_dtype=torch.float16, cache_dir=cache_dir
    ).to(device)
    model.eval()
    logger.info("CLIP loaded on %s", device)

    try:
        embeddings = _encode_crops(items_with_crop, processor, model, device)
        clusters = _cluster(items_with_crop, embeddings)
        result = _merge(all_items, items_with_crop, clusters)
    finally:
        del model
        del processor
        if torch.cuda.is_available():
            torch.cuda.empty_cache()
        logger.info("CLIP unloaded")

    return result


def _encode_crops(
    items_with_crop: list[tuple[int, DetectedItem]],
    processor,
    model,
    device: torch.device,
    batch_size: int = 32,
) -> np.ndarray:
    """Encode all crop thumbnails to L2-normalised CLIP embeddings.

    Called by: _run()
    Purpose: Produces the embedding matrix used for pairwise similarity.

    Returns:
        Float32 numpy array of shape (N, embedding_dim).
    """
    from PIL import Image

    crops = []
    for _, item in items_with_crop:
        data = base64.b64decode(item.crop_base64)  # type: ignore[arg-type]
        img = Image.open(io.BytesIO(data)).convert("RGB")
        crops.append(img)

    all_emb: list[np.ndarray] = []
    for i in range(0, len(crops), batch_size):
        batch = crops[i : i + batch_size]
        inputs = processor(images=batch, return_tensors="pt", padding=True).to(device)
        with torch.no_grad():
            feats = model.get_image_features(**inputs).float()
            feats = feats / feats.norm(dim=-1, keepdim=True)
        all_emb.append(feats.cpu().numpy())

    matrix = np.vstack(all_emb)  # (N, dim)
    logger.info("CLIP dedup: encoded %d crops → embeddings %s", len(crops), matrix.shape)
    return matrix


def _cluster(
    items_with_crop: list[tuple[int, DetectedItem]],
    embeddings: np.ndarray,
) -> list[list[int]]:
    """Greedy clustering by cosine similarity, restricted to cross-image pairs.

    Called by: _run()
    Purpose: Groups items that are the same physical object across photos.

    Rule: two items are candidates for merging only if they share NO source
    image — same-image items already went through within-image dedup and
    should never be merged here.

    Returns:
        List of clusters, each cluster is a list of local indices into
        items_with_crop.  Singletons are included (clusters of size 1).
    """
    sim_matrix: np.ndarray = embeddings @ embeddings.T  # (N, N), cosine sim

    n = len(items_with_crop)
    cluster_of = [-1] * n
    clusters: list[list[int]] = []

    for i in range(n):
        _, item_i = items_with_crop[i]
        imgs_i = set(item_i.seen_in_images)

        best_cluster = -1
        best_sim = _SIMILARITY_THRESHOLD

        for cid, cluster in enumerate(clusters):
            # Compare to every member, take max similarity
            max_sim = max(float(sim_matrix[i, j]) for j in cluster)
            if max_sim <= best_sim:
                continue

            # Reject if ANY member shares a source image with item_i
            shares_image = any(
                imgs_i & set(items_with_crop[j][1].seen_in_images)
                for j in cluster
            )
            if shares_image:
                continue

            best_sim = max_sim
            best_cluster = cid

        if best_cluster >= 0:
            clusters[best_cluster].append(i)
            cluster_of[i] = best_cluster
        else:
            cluster_of[i] = len(clusters)
            clusters.append([i])

    n_merged = sum(1 for c in clusters if len(c) > 1)
    logger.info(
        "CLIP dedup: %d items → %d clusters (%d multi-item, %d singletons)",
        n, len(clusters), n_merged, len(clusters) - n_merged,
    )
    return clusters


def _merge(
    all_items: list[DetectedItem],
    items_with_crop: list[tuple[int, DetectedItem]],
    clusters: list[list[int]],
) -> list[DetectedItem]:
    """Build the deduplicated item list from cluster assignments.

    Called by: _run()
    Purpose: For each multi-item cluster, keep the highest-confidence
             detection and merge seen_in_images to preserve provenance.

    Returns:
        Deduplicated DetectedItem list.
    """
    to_drop: set[int] = set()
    updates: dict[int, DetectedItem] = {}

    for cluster in clusters:
        if len(cluster) == 1:
            continue

        group = [(items_with_crop[li][0], items_with_crop[li][1]) for li in cluster]
        best_orig, best = max(group, key=lambda x: x[1].confidence)
        all_seen = sorted({img for _, item in group for img in item.seen_in_images})

        for orig_idx, item in group:
            if orig_idx == best_orig:
                continue
            to_drop.add(orig_idx)
            logger.debug(
                "CLIP dedup: drop %r (images %s, conf=%.2f) → merged into %r (images %s)",
                item.name, item.seen_in_images, item.confidence,
                best.name, all_seen,
            )

        updates[best_orig] = DetectedItem(
            name=best.name,
            volume_m3=best.volume_m3,
            dimensions=best.dimensions,
            confidence=best.confidence,
            seen_in_images=all_seen,
            category=best.category,
            german_name=best.german_name,
            re_value=best.re_value,
            units=best.units,
            volume_source=best.volume_source,
            bbox=best.bbox,
            bbox_image_index=best.bbox_image_index,
            crop_base64=best.crop_base64,
            is_moveable=best.is_moveable,
            packs_into_boxes=best.packs_into_boxes,
        )

    result: list[DetectedItem] = []
    for orig_idx, item in enumerate(all_items):
        if orig_idx in to_drop:
            continue
        result.append(updates.get(orig_idx, item))

    logger.info(
        "CLIP dedup complete: %d → %d items (%d removed)",
        len(all_items), len(result), len(to_drop),
    )
    return result
