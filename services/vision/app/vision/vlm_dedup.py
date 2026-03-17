"""VLM-based cross-image deduplication using Qwen2-VL-7B-Instruct.

After the standard detection + within-image dedup pipeline, this module
sends item crop thumbnails to the locally-loaded Qwen2-VL-7B to identify
items that are the same physical object photographed from different angles,
and optionally corrects misclassified labels.

GPU budget on L4 (24GB):
  Photo pipeline (DINO + SAM 2 + DA)  ~8GB
  Qwen2-VL-7B-Instruct (BF16)        ~14GB
  ─────────────────────────────────────────
  Total                               ~22GB  (fits with 2GB headroom)

No model swapping needed — all models stay resident simultaneously.
"""
from __future__ import annotations

import base64
import io
import json
import logging
import re
from typing import TYPE_CHECKING

import torch

if TYPE_CHECKING:
    from app.vision.model_loader import ModelRegistry

from app.models.schemas import DetectedItem, classify_item, lookup_re_volume

logger = logging.getLogger(__name__)

# Cap items per VLM call to control latency and context window size.
# CLIP dedup runs first and reduces item count to ~30-60, so this cap is
# rarely hit. Set to 60 to handle the post-CLIP list in a single pass.
_MAX_ITEMS_PER_CALL = 60


def vlm_dedup(
    items: list[DetectedItem],
    registry: ModelRegistry,
) -> list[DetectedItem]:
    """Cross-image dedup and label correction using the resident Qwen2-VL-7B.

    Caller: VisionPipeline.run() — Stage 6, after within-image dedup.
    Why: The standard dedup only merges overlapping boxes in the same photo.
         The same sofa photographed from three angles becomes three items and
         triples the volume estimate. Qwen2-VL compares crop thumbnails and
         identifies which items are the same physical object.

    Returns items unchanged if:
    - Qwen2-VL model is not loaded in the registry
    - No item label appears across more than one image (no cross-image dups possible)
    - The VLM call fails for any reason (graceful fallback — never raises)

    Args:
        items:    DetectedItem list from the standard within-image dedup step.
        registry: ModelRegistry holding the loaded Qwen2-VL model and processor.

    Returns:
        Deduplicated (and possibly relabeled) DetectedItem list.
    """
    if registry.qwen_vlm_model is None or registry.qwen_vlm_processor is None:
        logger.warning("Qwen2-VL not loaded — skipping cross-image VLM dedup")
        return items

    if not _has_cross_image_candidates(items):
        logger.info("VLM dedup: no cross-image label overlap — skipping")
        return items

    # Split items into those with crop thumbnails (can be compared) and those without
    items_with_crop: list[tuple[int, DetectedItem]] = [
        (i, item) for i, item in enumerate(items) if item.crop_base64
    ]

    if not items_with_crop:
        logger.info("VLM dedup: no crop thumbnails available — skipping")
        return items

    if len(items_with_crop) > _MAX_ITEMS_PER_CALL:
        logger.info(
            "VLM dedup: capping to %d items (total %d)",
            _MAX_ITEMS_PER_CALL, len(items_with_crop),
        )
        items_with_crop = items_with_crop[:_MAX_ITEMS_PER_CALL]

    try:
        duplicate_groups, relabels = _run_vlm_call(items_with_crop, registry)
    except Exception:
        logger.exception("VLM dedup call raised — returning items unchanged")
        return items

    return _apply_results(items, items_with_crop, duplicate_groups, relabels)


# ---------------------------------------------------------------------------
# Internals
# ---------------------------------------------------------------------------

def _has_cross_image_candidates(items: list[DetectedItem]) -> bool:
    """Return True if detected items span more than one source image.

    Intentionally label-agnostic: a sofa detected as 'sofa' in photo 1
    and 'couch' in photo 3 is still a cross-image duplicate even though
    the labels differ. The VLM decides same-object identity from the crops.
    """
    all_images = {img for item in items for img in item.seen_in_images}
    return len(all_images) > 1


def _b64_to_pil(b64_str: str):
    """Decode a base64-encoded JPEG thumbnail to a PIL Image."""
    from PIL import Image
    return Image.open(io.BytesIO(base64.b64decode(b64_str))).convert("RGB")


def _run_vlm_call(
    indexed_items: list[tuple[int, DetectedItem]],
    registry: ModelRegistry,
) -> tuple[list[list[int]], dict[str, str]]:
    """Build the Qwen2-VL prompt, run inference, return parsed results.

    Caller: vlm_dedup()
    Why: Encapsulates all model-specific API calls so vlm_dedup() stays clean.

    Args:
        indexed_items: (original_list_index, DetectedItem) pairs with crops.
        registry:      ModelRegistry with loaded Qwen2-VL model + processor.

    Returns:
        duplicate_groups: list of lists of local indices (into indexed_items).
        relabels:         dict of local_index_str -> corrected label string.
    """
    from qwen_vl_utils import process_vision_info

    crops = [_b64_to_pil(item.crop_base64) for _, item in indexed_items]  # type: ignore[arg-type]

    # Build content: one image block per item, followed by the task description
    content: list[dict] = []
    item_lines: list[str] = []

    for local_idx, (_, item) in enumerate(indexed_items):
        sources = ", ".join(f"photo {i + 1}" for i in item.seen_in_images)
        item_lines.append(
            f"  Item {local_idx}: \"{item.name}\" "
            f"(seen in {sources}, confidence {item.confidence:.0%})"
        )
        content.append({"type": "image", "image": crops[local_idx]})

    content.append({
        "type": "text",
        "text": (
            "You are helping a German moving company estimate furniture volume.\n"
            "The images above are cropped thumbnails of furniture items detected in moving photos.\n\n"
            "Detected items (one thumbnail each, in order):\n"
            + "\n".join(item_lines)
            + "\n\n"
            "TASK 1 — DEDUPLICATION:\n"
            "Which items are the SAME physical object photographed from different photos or angles? "
            "Only group items that are clearly the identical object in the same room — "
            "do NOT group items that are merely the same type of furniture.\n\n"
            "TASK 2 — LABEL CORRECTION (optional):\n"
            "If an item's label is clearly wrong based on its thumbnail, provide the correct English label. "
            "Only correct obvious misclassifications.\n\n"
            "Respond ONLY with a single JSON object and nothing else:\n"
            "{\"duplicate_groups\": [[0, 2], [1, 3]], \"relabels\": {\"4\": \"armchair\"}}\n"
            "If there are no duplicates or corrections: {\"duplicate_groups\": [], \"relabels\": {}}"
        ),
    })

    messages = [{"role": "user", "content": content}]

    processor = registry.qwen_vlm_processor
    model = registry.qwen_vlm_model

    text = processor.apply_chat_template(
        messages, tokenize=False, add_generation_prompt=True
    )
    image_inputs, video_inputs = process_vision_info(messages)

    inputs = processor(
        text=[text],
        images=image_inputs,
        videos=video_inputs,
        padding=True,
        return_tensors="pt",
    ).to(registry.device)

    with torch.no_grad():
        output_ids = model.generate(
            **inputs,
            max_new_tokens=256,
            do_sample=False,
            temperature=None,
            top_p=None,
        )

    # Decode only the newly generated tokens (strip the input prompt)
    input_len = inputs["input_ids"].shape[1]
    response_text = processor.batch_decode(
        output_ids[:, input_len:],
        skip_special_tokens=True,
        clean_up_tokenization_spaces=False,
    )[0].strip()

    logger.info("Qwen2-VL raw response: %.300s", response_text)
    return _parse_response(response_text, len(indexed_items))


def _parse_response(
    text: str,
    n_items: int,
) -> tuple[list[list[int]], dict[str, str]]:
    """Parse the VLM response into validated (duplicate_groups, relabels).

    Caller: _run_vlm_call()
    Why: The model sometimes wraps JSON in markdown or adds explanation text;
         this parser is deliberately tolerant of such noise.

    Args:
        text:    Raw text response from the model.
        n_items: Number of items sent to the model (for index validation).

    Returns:
        (duplicate_groups, relabels) — both empty on any parse failure.
    """
    json_match = re.search(r'\{[\s\S]*\}', text)
    if not json_match:
        logger.warning("VLM response has no JSON — skipping dedup")
        return [], {}

    try:
        data = json.loads(json_match.group())
    except json.JSONDecodeError as exc:
        logger.warning("VLM JSON parse error: %s — skipping dedup", exc)
        return [], {}

    # Validate duplicate_groups
    valid_groups: list[list[int]] = []
    for group in data.get("duplicate_groups", []):
        if not isinstance(group, list):
            continue
        idxs = [i for i in group if isinstance(i, int) and 0 <= i < n_items]
        if len(idxs) >= 2:
            valid_groups.append(idxs)

    # Validate relabels
    valid_relabels: dict[str, str] = {}
    for k, v in data.get("relabels", {}).items():
        try:
            idx = int(k)
        except (TypeError, ValueError):
            continue
        if 0 <= idx < n_items and isinstance(v, str) and v.strip():
            valid_relabels[str(idx)] = v.strip().lower()

    logger.info(
        "VLM parsed: %d duplicate groups, %d relabels",
        len(valid_groups), len(valid_relabels),
    )
    return valid_groups, valid_relabels


def _apply_results(
    all_items: list[DetectedItem],
    items_with_crop: list[tuple[int, DetectedItem]],
    duplicate_groups: list[list[int]],
    relabels: dict[str, str],
) -> list[DetectedItem]:
    """Apply VLM dedup results to the full item list.

    Caller: vlm_dedup()
    Why: Separates the mutation logic from the inference logic.

    For each duplicate group: keep the highest-confidence item, merge
    seen_in_images across all members, drop the rest.
    For relabels: update name/category, re-run RE lookup so volume stays
    consistent with the corrected label.

    Args:
        all_items:       Original full item list (indexed by position).
        items_with_crop: (orig_idx, item) pairs that were sent to the VLM.
        duplicate_groups: Groups of local indices (into items_with_crop).
        relabels:         local_index_str -> corrected label.

    Returns:
        Cleaned DetectedItem list (no duplicates, labels corrected).
    """
    to_drop: set[int] = set()
    updates: dict[int, DetectedItem] = {}

    # --- Deduplication ---
    for group in duplicate_groups:
        orig_indices = [items_with_crop[li][0] for li in group]
        group_items  = [items_with_crop[li][1] for li in group]

        best_pos = max(range(len(group_items)), key=lambda i: group_items[i].confidence)
        best_orig = orig_indices[best_pos]
        best = group_items[best_pos]
        all_seen = sorted({img for item in group_items for img in item.seen_in_images})

        for pos, orig_idx in enumerate(orig_indices):
            if pos != best_pos:
                to_drop.add(orig_idx)
                logger.info(
                    "VLM dedup: dropping %r (orig_idx=%d) as duplicate of orig_idx=%d",
                    group_items[pos].name, orig_idx, best_orig,
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
        )

    # --- Label corrections ---
    for local_idx_str, new_label in relabels.items():
        local_idx = int(local_idx_str)
        orig_idx, item = items_with_crop[local_idx]

        if orig_idx in to_drop:
            continue  # Item already removed as a duplicate

        base = updates.get(orig_idx, item)
        re_result = lookup_re_volume(new_label)

        if re_result:
            vol_m3, re_total, units, german_name = re_result
            updated = DetectedItem(
                name=new_label,
                volume_m3=vol_m3,
                dimensions=base.dimensions,
                confidence=base.confidence,
                seen_in_images=base.seen_in_images,
                category=classify_item(new_label),
                german_name=german_name,
                re_value=float(re_total),
                units=units,
                volume_source="re",
                bbox=base.bbox,
                bbox_image_index=base.bbox_image_index,
                crop_base64=base.crop_base64,
            )
        else:
            updated = DetectedItem(
                name=new_label,
                volume_m3=base.volume_m3,
                dimensions=base.dimensions,
                confidence=base.confidence,
                seen_in_images=base.seen_in_images,
                category=classify_item(new_label),
                german_name=base.german_name,
                re_value=base.re_value,
                units=base.units,
                volume_source=base.volume_source,
                bbox=base.bbox,
                bbox_image_index=base.bbox_image_index,
                crop_base64=base.crop_base64,
            )

        updates[orig_idx] = updated
        logger.info("VLM relabel: %r -> %r (orig_idx=%d)", item.name, new_label, orig_idx)

    # --- Build final list ---
    result: list[DetectedItem] = []
    for orig_idx, item in enumerate(all_items):
        if orig_idx in to_drop:
            continue
        result.append(updates.get(orig_idx, item))

    logger.info(
        "VLM dedup complete: %d -> %d items (%d removed)",
        len(all_items), len(result), len(to_drop),
    )
    return result
