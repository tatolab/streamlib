# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The record `CyberpunkAvatar` publishes on `pose_to_downstream`.

Its own module so a reader — the LeRobot recorder — imports the shape without
importing the tracker and the GL stage that produce it.
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict, get_args

__all__ = [
    "POSE_PROVENANCES",
    "SOLVED_POSE_RECORD_NAMES",
    "PoseProvenance",
    "SolvedPoseRecord",
    "solved_pose_record_from_records_message",
]

PoseProvenance = Literal["detected", "held", "idle"]
POSE_PROVENANCES: tuple[PoseProvenance, ...] = get_args(PoseProvenance)


class SolvedPoseRecord(TypedDict):
    """One camera frame's settled pose, keyed to that frame's capture stamp."""

    timestamp_ns: int
    provenance: PoseProvenance
    joints: dict[str, list[float]]
    visibility: dict[str, float]


SOLVED_POSE_RECORD_NAMES: tuple[str, ...] = tuple(SolvedPoseRecord.__annotations__)
"""The names a `pose_to_downstream` Records message carries."""


def solved_pose_record_from_records_message(message: dict[str, Any]) -> SolvedPoseRecord:
    """A Records message read back as the record, its provenance checked.

    A Records write checks only that every declared name is present, so the
    values are checked here, where a reader first relies on them.
    """
    provenance = message["provenance"]
    if provenance not in POSE_PROVENANCES:
        raise ValueError(
            f"a solved pose record's provenance is one of {POSE_PROVENANCES}, got {provenance!r}"
        )
    return {
        "timestamp_ns": int(message["timestamp_ns"]),
        "provenance": provenance,
        "joints": {
            name: [float(axis) for axis in joint] for name, joint in message["joints"].items()
        },
        "visibility": {name: float(seen) for name, seen in message["visibility"].items()},
    }
