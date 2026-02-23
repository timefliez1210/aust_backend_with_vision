from __future__ import annotations

import logging
import os
import tempfile
from dataclasses import dataclass

import numpy as np
import torch
from PIL import Image

logger = logging.getLogger(__name__)


class ReconstructionError(Exception):
    """Raised when MASt3R reconstruction fails or produces insufficient quality."""


@dataclass
class ReconstructionResult:
    """Result of MASt3R 3D reconstruction."""

    point_cloud: np.ndarray         # (N, 3) metric-scale world-frame points
    point_colors: np.ndarray        # (N, 3) RGB [0,1]
    camera_poses: list[np.ndarray]  # 4x4 cam-to-world per keyframe
    intrinsics: list[np.ndarray]    # 3x3 per keyframe
    confidence: np.ndarray          # (N,) per-point confidence
    point_to_image: np.ndarray      # (N,) source keyframe index per point
    depth_maps: list[np.ndarray]    # (H, W) per keyframe (from point projection)
    image_sizes: list[tuple[int, int]]  # (W, H) per keyframe


class Reconstructor:
    """MASt3R-based multi-view 3D reconstructor.

    Takes a set of keyframe images and produces a metric-scale point cloud
    with camera poses and per-point provenance tracking.
    """

    def __init__(self, model, device: torch.device) -> None:
        self._model = model
        self._device = device

    def reconstruct(
        self,
        images: list[Image.Image],
        min_conf: float = 1.5,
        niter: int = 300,
    ) -> ReconstructionResult:
        """Run multi-view reconstruction on keyframe images.

        Args:
            images: List of RGB PIL images (keyframes).
            min_conf: Minimum confidence threshold for filtering points.
            niter: Number of global alignment iterations.

        Returns:
            ReconstructionResult with metric point cloud and camera data.

        Raises:
            ReconstructionError: If reconstruction fails or produces
                insufficient quality (< 1000 confident points).
        """
        from dust3r.cloud_opt import GlobalAlignerMode, global_aligner
        from dust3r.inference import inference
        from dust3r.utils.image import load_images

        n = len(images)
        if n < 2:
            raise ReconstructionError("Need at least 2 images for reconstruction")

        logger.info("Starting MASt3R reconstruction with %d images", n)

        # Save images to temp dir for MASt3R's load_images
        with tempfile.TemporaryDirectory() as tmpdir:
            image_paths = []
            for i, img in enumerate(images):
                path = os.path.join(tmpdir, f"frame_{i:04d}.jpg")
                img.save(path, "JPEG", quality=95)
                image_paths.append(path)

            imgs = load_images(image_paths, size=512)

        # Create pairs: sequential (i, i+1) + skip-1 (i, i+2) for loop closure
        pairs = []
        for i in range(n - 1):
            pairs.append((imgs[i], imgs[i + 1]))
        for i in range(n - 2):
            pairs.append((imgs[i], imgs[i + 2]))

        logger.info("Running pairwise inference on %d pairs", len(pairs))

        output = inference(pairs, self._model, self._device, batch_size=1)

        scene = global_aligner(
            output, device=self._device, mode=GlobalAlignerMode.PointCloudOptimizer
        )
        loss = scene.compute_global_alignment(
            init="mst", niter=niter, schedule="cosine"
        )
        logger.info("Global alignment done, final loss: %.4f", float(loss))

        # Extract results
        pts3d = scene.get_pts3d()        # list of (H, W, 3) tensors
        confs = scene.get_conf()         # list of (H, W) tensors
        intrinsics_tensor = scene.get_intrinsics()  # (N, 3, 3)
        poses_tensor = scene.get_im_poses()         # (N, 4, 4)

        intrinsics_np = intrinsics_tensor.detach().cpu().numpy()
        poses_np = poses_tensor.detach().cpu().numpy()

        # Build merged point cloud with provenance tracking
        all_points = []
        all_colors = []
        all_conf = []
        all_source = []
        depth_maps = []

        for i in range(n):
            pts = pts3d[i].detach().cpu().numpy()        # (H, W, 3)
            conf = confs[i].detach().cpu().numpy()       # (H, W)
            h, w = pts.shape[:2]

            # Depth map: transform world points to camera frame, extract z
            cam2world = poses_np[i]
            world2cam = np.linalg.inv(cam2world)
            pts_flat = pts.reshape(-1, 3)
            pts_cam = (world2cam[:3, :3] @ pts_flat.T + world2cam[:3, 3:]).T
            depth_map = pts_cam[:, 2].reshape(h, w)
            depth_maps.append(depth_map)

            # Filter by confidence
            mask = conf > min_conf
            valid_pts = pts[mask]
            valid_conf = conf[mask]

            # Get colors from original images (resized to match pts3d resolution)
            img_resized = images[i].resize((w, h), Image.LANCZOS)
            img_array = np.array(img_resized).astype(np.float32) / 255.0
            valid_colors = img_array[mask]

            all_points.append(valid_pts)
            all_colors.append(valid_colors)
            all_conf.append(valid_conf)
            all_source.append(np.full(len(valid_pts), i, dtype=np.int32))

        point_cloud = np.concatenate(all_points, axis=0)
        point_colors = np.concatenate(all_colors, axis=0)
        conf_array = np.concatenate(all_conf, axis=0)
        source_array = np.concatenate(all_source, axis=0)

        total_pts = sum(c.numel() for c in confs)
        logger.info(
            "Reconstruction: %d confident points from %d total (%d images)",
            len(point_cloud), total_pts, n,
        )

        if len(point_cloud) < 1000:
            raise ReconstructionError(
                f"Insufficient quality: only {len(point_cloud)} confident points "
                f"(need >= 1000)"
            )

        return ReconstructionResult(
            point_cloud=point_cloud,
            point_colors=point_colors,
            camera_poses=[poses_np[i] for i in range(n)],
            intrinsics=[intrinsics_np[i] for i in range(n)],
            confidence=conf_array,
            point_to_image=source_array,
            depth_maps=depth_maps,
            image_sizes=[(img.width, img.height) for img in images],
        )

    def unload(self) -> None:
        """Free GPU memory used by MASt3R model."""
        del self._model
        self._model = None
        if torch.cuda.is_available():
            torch.cuda.empty_cache()
        logger.info("MASt3R model unloaded from GPU")
