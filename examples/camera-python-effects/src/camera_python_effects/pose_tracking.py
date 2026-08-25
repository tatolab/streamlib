# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""MediaPipe pose, narrowed to the one question the rig asks.

BlazePose answers 33 landmarks with real depth — world coordinates in metres,
origin between the hips — which is what lets a monocular camera drive a 3D
body: joint *positions* in space, not just screen dots. It runs on the CPU in
a few milliseconds, so there is no CUDA stack to disagree with. The lite model
(~5.5 MB) is fetched once into a cache directory on first use; a
`pose_model_path` pointing at a local `.task` file skips the network entirely.
"""

from __future__ import annotations

import urllib.request
from dataclasses import dataclass
from pathlib import Path

import mediapipe
import numpy
from mediapipe.tasks.python import vision
from mediapipe.tasks.python.core.base_options import BaseOptions

from .avatar_rig import joints_from_world_landmarks, visibilities_from_world_landmarks

__all__ = ["PoseSample", "PoseTracker", "resolve_pose_model_file"]

POSE_MODEL_URL = (
    "https://storage.googleapis.com/mediapipe-models/pose_landmarker/"
    "pose_landmarker_lite/float16/latest/pose_landmarker_lite.task"
)
POSE_MODEL_CACHE_DIRECTORY = Path.home() / ".cache" / "camera-python-effects"


def resolve_pose_model_file(explicit_model_path: "str | None" = None) -> Path:
    """The `.task` model file, downloaded into the cache when absent."""
    if explicit_model_path is not None:
        return Path(explicit_model_path)
    cached_model = POSE_MODEL_CACHE_DIRECTORY / "pose_landmarker_lite.task"
    if not cached_model.is_file():
        POSE_MODEL_CACHE_DIRECTORY.mkdir(parents=True, exist_ok=True)
        # Fetched to a temporary name so a cut-off download never poses as a
        # complete model on the next run.
        half_fetched = cached_model.with_suffix(".task.downloading")
        urllib.request.urlretrieve(POSE_MODEL_URL, half_fetched)
        half_fetched.replace(cached_model)
    return cached_model


@dataclass
class PoseSample:
    """One person's joints, and how much each one was actually seen."""

    joints: "dict[str, numpy.ndarray]"
    visibility: "dict[str, float]"


class PoseTracker:
    """One person's world-space joints out of an RGB frame, or None."""

    def __init__(
        self,
        detection_confidence: float = 0.5,
        model_path: "str | None" = None,
    ) -> None:
        self._landmarker = vision.PoseLandmarker.create_from_options(
            vision.PoseLandmarkerOptions(
                base_options=BaseOptions(
                    model_asset_path=str(resolve_pose_model_file(model_path))
                ),
                running_mode=vision.RunningMode.VIDEO,
                min_pose_detection_confidence=detection_confidence,
                min_tracking_confidence=0.5,
            )
        )

    def world_joints(
        self, rgb_frame: numpy.ndarray, timestamp_ms: int
    ) -> "PoseSample | None":
        """The rig's joint set for the most confident person, mirrored.

        `rgb_frame` is HxWx3 uint8; `timestamp_ms` must increase between
        calls — video mode tracks across frames, and the camera's monotonic
        stamp is exactly that. None when nobody clears the confidence floor —
        the caller decides what an empty stage shows.

        Visibility rides along per joint: the detector answers a position for
        every landmark, seen or hallucinated, and downstream needs to know
        which is which.
        """
        detection = self._landmarker.detect_for_video(
            mediapipe.Image(
                image_format=mediapipe.ImageFormat.SRGB,
                data=numpy.ascontiguousarray(rgb_frame),
            ),
            timestamp_ms,
        )
        if not detection.pose_world_landmarks:
            return None
        landmarks = detection.pose_world_landmarks[0]
        return PoseSample(
            joints=joints_from_world_landmarks(landmarks),
            visibility=visibilities_from_world_landmarks(landmarks),
        )

    def close(self) -> None:
        self._landmarker.close()
