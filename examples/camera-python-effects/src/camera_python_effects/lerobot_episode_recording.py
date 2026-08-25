# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Writes what the pipeline sees and solves into a LeRobotDataset.

The bridge to the physical-AI ecosystem: LeRobot's dataset format is the
lingua franca of imitation learning, and this module speaks it through the
official writer — `LeRobotDataset.create` / `add_frame` / `save_episode` — so
what lands on disk is valid by construction and loads in every tool that
speaks LeRobot: their loaders, their visualizers, their training scripts.

The schema, per frame:
- `observation.images.camera` — the raw camera view, video-encoded
- `observation.images.stylized` — the composited pipeline output (optional)
- `observation.state` — the solved pose: xyz per joint, scene space, metres
- `observation.pose_visibility` — how much of each joint was actually seen
- `observation.capture_timestamp_ns` — the camera's monotonic stamp, so the
  true capture cadence survives even though the dataset's own timestamps are
  the regular frame grid LeRobot expects
- `action` — the same pose vector: the target a retargeting consumer servos
  toward, which is what a teleoperation recording means by action

Kept streamlib-free so the format contract is testable with nothing but a
temporary directory.
"""

from __future__ import annotations

from pathlib import Path

import numpy

from .avatar_rig import JOINT_NAMES

__all__ = ["LeRobotEpisodeRecording", "pose_bag_to_vectors"]

AXES = ("x", "y", "z")


def pose_bag_to_vectors(
    pose_bag: "dict",
) -> "tuple[numpy.ndarray, numpy.ndarray]":
    """A pose bag's joints and visibility as fixed-layout float32 vectors.

    Layout is `JOINT_NAMES` order, xyz per joint — the order the feature
    names in the dataset schema promise.
    """
    joints = pose_bag["joints"]
    visibility = pose_bag.get("visibility", {})
    state = numpy.array(
        [axis for name in JOINT_NAMES for axis in joints[name]],
        dtype=numpy.float32,
    )
    seen = numpy.array(
        [float(visibility.get(name, 0.0)) for name in JOINT_NAMES],
        dtype=numpy.float32,
    )
    return state, seen


class LeRobotEpisodeRecording:
    """One recording session: frames in, episodes out, a dataset on close."""

    def __init__(
        self,
        root: "str | Path",
        repo_id: str,
        fps: int,
        camera_height: int,
        camera_width: int,
        task: str,
        record_stylized: bool = True,
    ) -> None:
        # Imported here, not at module top: the wheel-heavy dependency loads
        # in the recorder's own helper process and nowhere else.
        from lerobot.datasets.lerobot_dataset import LeRobotDataset

        self.task = task
        self.record_stylized = record_stylized
        state_names = [f"{name}.{axis}" for name in JOINT_NAMES for axis in AXES]
        image_shape = (camera_height, camera_width, 3)
        image_axis_names = ["height", "width", "channels"]
        features = {
            "observation.images.camera": {
                "dtype": "video", "shape": image_shape, "names": image_axis_names,
            },
            "observation.state": {
                "dtype": "float32", "shape": (len(state_names),), "names": state_names,
            },
            "observation.pose_visibility": {
                "dtype": "float32",
                "shape": (len(JOINT_NAMES),),
                "names": list(JOINT_NAMES),
            },
            "observation.capture_timestamp_ns": {
                "dtype": "int64", "shape": (1,), "names": ["monotonic_ns"],
            },
            "action": {
                "dtype": "float32", "shape": (len(state_names),), "names": state_names,
            },
        }
        if record_stylized:
            features["observation.images.stylized"] = {
                "dtype": "video", "shape": image_shape, "names": image_axis_names,
            }
        self._dataset = LeRobotDataset.create(
            repo_id, fps=fps, features=features, root=Path(root)
        )
        self.frames_in_current_episode = 0
        self.episodes_saved = 0

    def add(
        self,
        pose_bag: "dict",
        camera_rgb: numpy.ndarray,
        stylized_rgb: "numpy.ndarray | None",
    ) -> None:
        state, seen = pose_bag_to_vectors(pose_bag)
        frame = {
            "observation.images.camera": camera_rgb,
            "observation.state": state,
            "observation.pose_visibility": seen,
            "observation.capture_timestamp_ns": numpy.array(
                [pose_bag["timestamp_ns"]], dtype=numpy.int64
            ),
            # A teleoperation recording's action is the pose the operator is
            # commanding — for a pure pose pilot, the state itself.
            "action": state,
            "task": self.task,
        }
        if self.record_stylized:
            frame["observation.images.stylized"] = (
                stylized_rgb if stylized_rgb is not None else camera_rgb
            )
        self._dataset.add_frame(frame)
        self.frames_in_current_episode += 1

    def save_episode(self) -> None:
        """Close the open episode; video encoding happens here."""
        if self.frames_in_current_episode == 0:
            return
        self._dataset.save_episode()
        self.frames_in_current_episode = 0
        self.episodes_saved += 1

    def close(self) -> None:
        """Save any open episode and seal the dataset's metadata."""
        self.save_episode()
        if self.episodes_saved > 0:
            self._dataset.finalize()
