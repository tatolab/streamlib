# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The record `CyberpunkAvatar` publishes on `pose_to_downstream`.

Its own module so a reader — the LeRobot recorder — imports the shape without
importing the tracker and the GL stage that produce it.
"""

from __future__ import annotations

from typing import Literal, TypedDict

__all__ = ["PoseProvenance", "SolvedPoseRecord"]

PoseProvenance = Literal["detected", "held", "idle"]


class SolvedPoseRecord(TypedDict):
    """One camera frame's settled pose, keyed to that frame's capture stamp."""

    timestamp_ns: int
    provenance: PoseProvenance
    joints: dict[str, list[float]]
    visibility: dict[str, float]
