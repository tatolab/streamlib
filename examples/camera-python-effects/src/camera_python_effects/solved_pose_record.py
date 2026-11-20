# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The pose `CyberpunkAvatar` publishes on `pose_to_downstream`: `SolvedPose`, the sample's own pin type.

Its own module so a reader — the LeRobot recorder — imports the shape without
importing the tracker and the GL stage that produce it.
"""

from __future__ import annotations

import json
from collections.abc import Buffer
from dataclasses import dataclass
from typing import Any, Literal, Self, TypedDict, get_args

from tatolab.stream import named_pin_type

__all__ = [
    "POSE_PROVENANCES",
    "PoseProvenance",
    "SolvedPose",
    "SolvedPoseRecord",
    "solved_pose_record_checked",
]

PoseProvenance = Literal["detected", "held", "idle"]
POSE_PROVENANCES: tuple[PoseProvenance, ...] = get_args(PoseProvenance)


class SolvedPoseRecord(TypedDict):
    """One camera frame's settled pose, keyed to that frame's capture stamp."""

    timestamp_ns: int
    provenance: PoseProvenance
    joints: dict[str, list[float]]
    visibility: dict[str, float]


def solved_pose_record_checked(message: dict[str, Any]) -> SolvedPoseRecord:
    """`message`, a JSON object read back, as the record, its provenance checked."""
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


@named_pin_type(
    description=(
        "One camera frame's settled pose: each joint's position and visibility, and whether it "
        "was detected, held through a dropout or idle."
    )
)
@dataclass(frozen=True)
class SolvedPose:
    """A `SolvedPoseRecord`, travelling between nodes as its JSON text."""

    record: SolvedPoseRecord

    @classmethod
    def from_bytes(cls, payload: Buffer) -> Self:
        return cls(solved_pose_record_checked(json.loads(bytes(payload))))

    def to_bytes(self) -> bytes:
        return json.dumps(self.record, separators=(",", ":"), allow_nan=False).encode()
