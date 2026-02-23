from __future__ import annotations

import logging
import os
import tempfile
from dataclasses import dataclass

import cv2
import numpy as np
from PIL import Image

logger = logging.getLogger(__name__)


@dataclass
class KeyframeResult:
    """Result of keyframe extraction from a video."""

    frames: list[Image.Image]
    frame_indices: list[int]
    fps: float
    total_frames: int
    width: int
    height: int


def extract_keyframes(
    video_bytes: bytes,
    max_frames: int = 20,
    min_frames: int = 10,
    blur_threshold: float = 100.0,
    min_interval_sec: float = 0.5,
) -> KeyframeResult:
    """Extract keyframes from a video based on scene changes.

    Selects frames with the largest inter-frame differences, spaced at least
    min_interval_sec apart, rejecting blurry frames. Always includes first
    and last frames.

    Args:
        video_bytes: Raw video file bytes (mp4, mov, webm, mkv).
        max_frames: Maximum number of keyframes to extract.
        min_frames: Minimum number of keyframes; fills with uniform sampling
            if filtering leaves fewer.
        blur_threshold: Laplacian variance threshold; frames below are blurry.
        min_interval_sec: Minimum time between selected keyframes.

    Returns:
        KeyframeResult with extracted frames and metadata.
    """
    with tempfile.NamedTemporaryFile(suffix=".mp4", delete=False) as tmp:
        tmp.write(video_bytes)
        tmp_path = tmp.name

    try:
        return _extract_from_file(
            tmp_path, max_frames, min_frames, blur_threshold, min_interval_sec
        )
    finally:
        os.unlink(tmp_path)


def _extract_from_file(
    video_path: str,
    max_frames: int,
    min_frames: int,
    blur_threshold: float,
    min_interval_sec: float,
) -> KeyframeResult:
    """Internal: extract keyframes from a video file on disk."""
    cap = cv2.VideoCapture(video_path)
    if not cap.isOpened():
        raise ValueError("Failed to open video file")

    fps = cap.get(cv2.CAP_PROP_FPS) or 30.0
    total_frames = int(cap.get(cv2.CAP_PROP_FRAME_COUNT))
    width = int(cap.get(cv2.CAP_PROP_FRAME_WIDTH))
    height = int(cap.get(cv2.CAP_PROP_FRAME_HEIGHT))
    min_interval_frames = max(1, int(min_interval_sec * fps))

    logger.info(
        "Video: %dx%d, %.1f fps, %d frames (%.1fs)",
        width, height, fps, total_frames, total_frames / fps,
    )

    # First pass: read all frames as grayscale, compute diffs + blur scores
    gray_frames: list[np.ndarray] = []
    while True:
        ret, frame = cap.read()
        if not ret:
            break
        gray_frames.append(cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY))
    cap.release()

    n = len(gray_frames)
    if n < 2:
        raise ValueError(f"Video too short: only {n} frames")

    # Inter-frame L1 difference
    diffs = np.zeros(n)
    for i in range(1, n):
        diffs[i] = np.mean(np.abs(
            gray_frames[i].astype(np.float32) - gray_frames[i - 1].astype(np.float32)
        ))

    # Blur score (Laplacian variance)
    blur_scores = np.array([
        cv2.Laplacian(g, cv2.CV_64F).var() for g in gray_frames
    ])
    del gray_frames  # free memory

    # Candidates: non-blurry frames
    candidates = set(i for i in range(n) if blur_scores[i] >= blur_threshold)
    # Always include first and last
    candidates.add(0)
    candidates.add(n - 1)

    # Sort candidates by scene change score (descending)
    candidates_scored = sorted(candidates, key=lambda i: diffs[i], reverse=True)

    # Greedily select frames with minimum spacing
    selected: list[int] = []
    for idx in candidates_scored:
        if len(selected) >= max_frames:
            break
        if all(abs(idx - s) >= min_interval_frames for s in selected):
            selected.append(idx)

    # If too few, fill with uniform sampling
    if len(selected) < min_frames:
        uniform = np.linspace(0, n - 1, min_frames, dtype=int).tolist()
        for idx in uniform:
            if idx not in selected and len(selected) < max_frames:
                selected.append(idx)

    selected.sort()
    blur_rejected = sum(1 for i in range(n) if blur_scores[i] < blur_threshold)
    logger.info(
        "Selected %d keyframes from %d total (blur rejected: %d)",
        len(selected), n, blur_rejected,
    )

    # Second pass: extract RGB frames for selected indices only
    cap = cv2.VideoCapture(video_path)
    selected_set = set(selected)
    frames: list[Image.Image] = []
    frame_idx = 0
    while True:
        ret, frame = cap.read()
        if not ret:
            break
        if frame_idx in selected_set:
            rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
            frames.append(Image.fromarray(rgb))
        frame_idx += 1
    cap.release()

    return KeyframeResult(
        frames=frames,
        frame_indices=selected,
        fps=fps,
        total_frames=n,
        width=width,
        height=height,
    )
