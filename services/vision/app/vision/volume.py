from __future__ import annotations

import logging
import math

import numpy as np
import open3d as o3d
from PIL import Image
from PIL.ExifTags import Base as ExifBase

from app.config import settings
from app.models.schemas import (
    Detection,
    ItemDimensions,
    VolumeEstimate,
    classify_item,
    get_max_volume,
    get_reference_dims,
    lookup_re_volume,
)

logger = logging.getLogger(__name__)

# Assumed horizontal FOV in degrees for a typical smartphone camera.
# Used as fallback when EXIF focal length is not available.
# ~70° matches iPhone main camera (~26mm equivalent).
DEFAULT_HFOV_DEG = 70.0

# Diagonal of a 35mm film frame in mm (sqrt(36² + 24²))
_35MM_DIAGONAL = 43.27


def extract_focal_length_35mm(image: Image.Image) -> float | None:
    """Extract 35mm-equivalent focal length from EXIF data.

    Most smartphone photos retain this metadata. Returns None if
    EXIF is stripped or the tag is missing.
    """
    try:
        exif = image.getexif()
        if not exif:
            return None

        # Try FocalLengthIn35mmFilm first (most reliable)
        fl_35mm = exif.get(ExifBase.FocalLengthIn35mmFilm)
        if fl_35mm and fl_35mm > 0:
            return float(fl_35mm)

        # Fallback: compute from FocalLength + sensor size (less common)
        fl = exif.get(ExifBase.FocalLength)
        if fl and fl > 0:
            # If we have FocalLength but not 35mm equiv, assume phone sensor
            # Typical phone sensor diagonal: ~7.7mm (1/2.3"), ~6.17mm (1/2.55")
            # Without exact sensor size, the 35mm equiv is unreliable.
            # Return None and use default HFOV instead.
            return None

    except Exception:
        pass

    return None


def compute_intrinsics(
    width: int,
    height: int,
    focal_length_35mm: float | None = None,
    hfov_deg: float = DEFAULT_HFOV_DEG,
) -> np.ndarray:
    """Compute a pinhole camera intrinsic matrix.

    If focal_length_35mm is available (from EXIF), uses it for precise
    focal length computation. Otherwise falls back to assumed HFOV.
    """
    if focal_length_35mm is not None and focal_length_35mm > 0:
        # Convert 35mm equivalent focal length to pixel focal length.
        # The 35mm equiv is defined relative to the diagonal of a 36×24mm frame.
        diagonal_pixels = math.sqrt(width**2 + height**2)
        fx = diagonal_pixels * focal_length_35mm / _35MM_DIAGONAL
        fy = fx  # square pixels
        hfov_actual = math.degrees(2 * math.atan(width / (2 * fx)))
        logger.info(
            "EXIF intrinsics: fl_35mm=%.1fmm → fx=%.1fpx, hfov=%.1f°",
            focal_length_35mm, fx, hfov_actual,
        )
    else:
        fx = width / (2.0 * np.tan(np.radians(hfov_deg / 2.0)))
        fy = fx
        logger.debug("Default intrinsics: hfov=%.1f° → fx=%.1fpx", hfov_deg, fx)

    cx = width / 2.0
    cy = height / 2.0
    return np.array([[fx, 0, cx], [0, fy, cy], [0, 0, 1]], dtype=np.float64)


def _crop_bbox_thumbnail(image_array: np.ndarray, bbox: list[float], max_width: int = 300) -> str:
    """Crop bbox region, resize to max_width, return base64 JPEG."""
    import base64
    import io

    h, w = image_array.shape[:2]
    x1 = max(0, int(bbox[0]))
    y1 = max(0, int(bbox[1]))
    x2 = min(w, int(bbox[2]))
    y2 = min(h, int(bbox[3]))
    if x2 <= x1 or y2 <= y1:
        return ""

    crop = image_array[y1:y2, x1:x2]
    pil_crop = Image.fromarray(crop)
    if pil_crop.width > max_width:
        ratio = max_width / pil_crop.width
        pil_crop = pil_crop.resize(
            (max_width, int(pil_crop.height * ratio)),
            Image.LANCZOS,
        )

    buf = io.BytesIO()
    pil_crop.save(buf, format="JPEG", quality=80)
    return base64.b64encode(buf.getvalue()).decode("ascii")


class VolumeCalculator:
    """Estimate volume of detected objects using depth maps and the RE catalog.

    Hybrid approach:
    1. Back-project masked depth to 3D → compute OBB for approximate dimensions
    2. Use dimensions to look up volume in the RE (Raumeinheit) catalog:
       - Fixed items: detection alone → RE lookup (e.g., chair = 2 RE = 0.2 m³)
       - Size variants: measure dimension → pick RE bracket (e.g., table width)
       - Per-unit items: measure width → count units (e.g., sofa seats)
    3. If item not in RE catalog: fall back to geometric OBB volume with
       scale calibration (existing behavior).

    RE volumes already include handling/packing space, so packing multipliers
    should NOT be applied to RE-sourced estimates.
    """

    def estimate_volumes(
        self,
        images: list[Image.Image],
        detections: list[Detection],
        masks_by_image: dict[int, list[np.ndarray]],
        depth_maps: list[np.ndarray],
    ) -> list[VolumeEstimate]:
        """Calculate volume estimates for all detections."""
        # Group detections by image (preserve order consistent with masks)
        grouped: dict[int, list[Detection]] = {}
        for det in detections:
            grouped.setdefault(det.image_index, []).append(det)

        results: list[VolumeEstimate] = []

        for img_idx, image in enumerate(images):
            dets = grouped.get(img_idx, [])
            masks = masks_by_image.get(img_idx, [])
            depth_map = depth_maps[img_idx]

            # Extract EXIF for better intrinsics
            fl_35mm = extract_focal_length_35mm(image)
            intrinsics = compute_intrinsics(image.width, image.height, fl_35mm)

            # First pass: compute raw estimates (dimensions + RE or geometric volume)
            raw_estimates = []
            for det, mask in zip(dets, masks):
                raw = self._estimate_single(
                    det, mask, depth_map, intrinsics, np.array(image)
                )
                raw_estimates.append(raw)

            # Scale calibration: only for geometric-sourced items (not RE)
            geometric_estimates = [
                (det, est) for det, est in zip(dets, raw_estimates)
                if est is not None and est.volume_source == "geometric"
            ]
            if geometric_estimates:
                geo_dets, geo_ests = zip(*geometric_estimates)
                scale = self._compute_scale_factor(list(geo_dets), list(geo_ests))
            else:
                scale = 1.0

            if abs(scale - 1.0) > 0.01:
                logger.info("Image %d: applying scale factor %.3f to geometric items", img_idx, scale)

            # Second pass: apply scale to geometric items, filter outliers
            for raw in raw_estimates:
                if raw is None:
                    continue

                if raw.volume_source == "geometric":
                    calibrated = self._apply_scale(raw, scale)
                else:
                    calibrated = raw  # RE items don't need scale calibration

                # Outlier filtering
                max_vol = get_max_volume(calibrated.category)
                if calibrated.volume_m3 > max_vol:
                    logger.warning(
                        "Outlier filtered: '%s' volume=%.3f m³ exceeds max %.1f m³ for category '%s'",
                        calibrated.label, calibrated.volume_m3, max_vol, calibrated.category,
                    )
                    continue

                max_dim = max(
                    calibrated.dimensions.length_m,
                    calibrated.dimensions.width_m,
                    calibrated.dimensions.height_m,
                )
                if max_dim > 4.0 and calibrated.volume_source == "geometric":
                    logger.warning(
                        "Outlier filtered: '%s' max_dim=%.3f m exceeds 4.0m",
                        calibrated.label, max_dim,
                    )
                    continue

                results.append(calibrated)

        logger.info("Volume estimates computed for %d objects", len(results))
        return results

    def _compute_scale_factor(
        self,
        detections: list[Detection],
        raw_estimates: list[VolumeEstimate | None],
    ) -> float:
        """Derive a linear scale factor by comparing estimated vs reference dimensions."""
        weighted_sum = 0.0
        weight_total = 0.0

        for det, est in zip(detections, raw_estimates):
            if est is None:
                continue

            ref = get_reference_dims(det.label)
            if ref is None:
                continue

            est_dims = sorted(
                [est.dimensions.length_m, est.dimensions.width_m, est.dimensions.height_m],
                reverse=True,
            )
            ref_dims = sorted(ref, reverse=True)

            if est_dims[0] > 0.001:
                ratio = ref_dims[0] / est_dims[0]
                if 0.1 < ratio < 20.0:
                    weight = det.confidence
                    weighted_sum += ratio * weight
                    weight_total += weight
                    logger.info(
                        "Scale ref: '%s' (conf=%.2f) est_max=%.3fm ref_max=%.3fm → ratio=%.3f",
                        det.label, det.confidence, est_dims[0], ref_dims[0], ratio,
                    )

        if weight_total > 0:
            avg_ratio = weighted_sum / weight_total
            logger.info("Weighted average scale factor: %.3f (from %.1f total weight)", avg_ratio, weight_total)
            return avg_ratio

        logger.warning("No known reference items found for scale calibration")
        return 1.0

    @staticmethod
    def _apply_scale(est: VolumeEstimate, scale: float) -> VolumeEstimate:
        """Apply a linear scale factor to dimensions and volume."""
        new_l = round(est.dimensions.length_m * scale, 3)
        new_w = round(est.dimensions.width_m * scale, 3)
        new_h = round(est.dimensions.height_m * scale, 3)
        new_vol = round(new_l * new_w * new_h, 4)

        return VolumeEstimate(
            label=est.label,
            volume_m3=new_vol,
            dimensions=ItemDimensions(length_m=new_l, width_m=new_w, height_m=new_h),
            confidence=est.confidence,
            image_index=est.image_index,
            category=est.category,
            feature_vector=est.feature_vector,
            volume_source=est.volume_source,
            re_value=est.re_value,
            german_name=est.german_name,
            units=est.units,
            bbox=est.bbox,
            crop_base64=est.crop_base64,
        )

    def _estimate_single(
        self,
        detection: Detection,
        mask: np.ndarray,
        depth_map: np.ndarray,
        intrinsics: np.ndarray,
        image_array: np.ndarray,
    ) -> VolumeEstimate | None:
        """Estimate volume for a single masked object.

        Uses depth for approximate dimensions, then:
        - If item is in RE catalog: use RE-based volume (with dimensions for disambiguation)
        - If not in catalog: use geometric OBB volume (existing behavior)
        """
        # Extract depth values within the mask
        masked_depth = depth_map.copy()
        masked_depth[~mask] = 0

        ys, xs = np.where(mask)
        if len(ys) < 10:
            logger.warning(
                "Too few mask pixels (%d) for '%s', skipping", len(ys), detection.label
            )
            return None

        depths = masked_depth[ys, xs]
        valid = depths > 0.01
        if valid.sum() < 10:
            logger.warning("Too few valid depth pixels for '%s', skipping", detection.label)
            return None

        ys = ys[valid]
        xs = xs[valid]
        depths = depths[valid]

        # Back-project to 3D
        points = self._backproject(xs, ys, depths, intrinsics)

        # Build Open3D point cloud
        pcd = o3d.geometry.PointCloud()
        pcd.points = o3d.utility.Vector3dVector(points)

        # Remove statistical outliers
        pcd, _ = pcd.remove_statistical_outlier(nb_neighbors=20, std_ratio=2.0)

        if len(pcd.points) < 10:
            logger.warning("Too few points after outlier removal for '%s'", detection.label)
            return None

        # Compute OBB for approximate dimensions
        try:
            obb = pcd.get_oriented_bounding_box()
            extent = obb.extent
            dims = sorted(extent.tolist(), reverse=True)
            obb_volume = float(np.prod(extent))
            centroid = np.asarray(obb.center).tolist()
        except Exception:
            logger.warning("OBB computation failed for '%s', using AABB", detection.label)
            aabb = pcd.get_axis_aligned_bounding_box()
            extent = aabb.get_extent()
            dims = sorted(extent.tolist(), reverse=True)
            obb_volume = float(np.prod(extent))
            centroid = np.asarray(aabb.get_center()).tolist()

        # Feature vector for deduplication (same as before)
        colors = image_array[ys, xs].astype(np.float32) / 255.0
        mean_color = colors.mean(axis=0).tolist()
        feature_vector = mean_color + centroid + dims

        category = classify_item(detection.label)

        # Also compute bbox-based dimensions as a simpler estimate.
        # For disambiguation, bbox-based is more stable because it preserves
        # the natural width/height orientation (OBB rotation can swap axes).
        bbox = detection.bbox  # [x1, y1, x2, y2]
        bbox_width_px = bbox[2] - bbox[0]
        bbox_height_px = bbox[3] - bbox[1]
        median_depth = float(np.median(depths))
        fx, fy = intrinsics[0, 0], intrinsics[1, 1]
        apparent_width_m = bbox_width_px * median_depth / fx
        apparent_height_m = bbox_height_px * median_depth / fy

        # Sort for "largest" and "second" dimension lookups
        apparent_dims = sorted([apparent_width_m, apparent_height_m], reverse=True)

        # Generate crop thumbnail
        crop_b64 = _crop_bbox_thumbnail(image_array, bbox)

        # Try RE catalog lookup
        re_result = lookup_re_volume(
            label=detection.label,
            largest_dim_m=apparent_dims[0],
            second_dim_m=apparent_dims[1],
            height_m=apparent_height_m,
        )

        if re_result is not None:
            volume_m3, re_total, units, german_name = re_result
            logger.info(
                "RE lookup: '%s' → %s (%.1f RE, %d unit(s), %.2f m³) "
                "[apparent: %.2fm × %.2fm, OBB: %.2fm × %.2fm × %.2fm = %.3f m³]",
                detection.label, german_name, re_total, units, volume_m3,
                apparent_dims[0], apparent_dims[1],
                dims[0], dims[1], dims[2], obb_volume,
            )

            return VolumeEstimate(
                label=detection.label,
                volume_m3=round(volume_m3, 4),
                dimensions=ItemDimensions(
                    length_m=round(dims[0], 3),
                    width_m=round(dims[1], 3),
                    height_m=round(dims[2], 3),
                ),
                confidence=detection.confidence,
                image_index=detection.image_index,
                category=category,
                feature_vector=feature_vector,
                volume_source="re",
                re_value=re_total,
                german_name=german_name,
                units=units,
                bbox=list(bbox),
                crop_base64=crop_b64,
            )

        # Fallback: geometric OBB volume (item not in RE catalog)
        logger.info(
            "Geometric fallback: '%s' OBB volume=%.4f m³, dims=%.2f×%.2f×%.2f",
            detection.label, obb_volume, dims[0], dims[1], dims[2],
        )

        return VolumeEstimate(
            label=detection.label,
            volume_m3=round(obb_volume, 4),
            dimensions=ItemDimensions(
                length_m=round(dims[0], 3),
                width_m=round(dims[1], 3),
                height_m=round(dims[2], 3),
            ),
            confidence=detection.confidence,
            image_index=detection.image_index,
            category=category,
            feature_vector=feature_vector,
            volume_source="geometric",
            bbox=list(bbox),
            crop_base64=crop_b64,
        )

    @staticmethod
    def _backproject(
        xs: np.ndarray, ys: np.ndarray, depths: np.ndarray, intrinsics: np.ndarray
    ) -> np.ndarray:
        """Back-project 2D pixel coordinates + depth to 3D points."""
        fx, fy = intrinsics[0, 0], intrinsics[1, 1]
        cx, cy = intrinsics[0, 2], intrinsics[1, 2]

        x3d = (xs.astype(np.float64) - cx) * depths / fx
        y3d = (ys.astype(np.float64) - cy) * depths / fy
        z3d = depths.astype(np.float64)

        return np.stack([x3d, y3d, z3d], axis=-1)
