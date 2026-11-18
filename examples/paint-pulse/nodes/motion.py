# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Function nodes that turn the listener's record and the stage's camera into pins.

`MotionReadout` unpacks the listener's motion record into Float pins, so built-ins such as
Lag, Remap, RgbSplit and HsvAdjust can follow the music. `OrbitCamera` folds the stage view's
drag and zoom into one camera record, adding the slow spin a Speed node accumulates.
"""

from __future__ import annotations

from dataclasses import dataclass

from tatolab.stream import node


@dataclass(frozen=True)
class MotionReading:
    """The music as numbers: the kick's pump, the overall energy, and the bass and highs levels."""

    beat: float
    energy: float
    bass: float
    highs: float


@node.meta(
    category="Paint Pulse",
    description="Unpack the listener's motion record into Float pins: beat, energy, bass and highs.",
)
@node.function
def MotionReadout(motion: dict | None = None) -> MotionReading:
    """Each named number of `motion`, or 0 before the first record arrives."""
    if motion is None:
        return MotionReading(0.0, 0.0, 0.0, 0.0)
    return MotionReading(
        float(motion.get("beat", 0.0)),
        float(motion.get("energy", 0.0)),
        float(motion.get("bass", 0.0)),
        float(motion.get("highs", 0.0)),
    )


@node.meta(
    category="Paint Pulse",
    description="The camera the ball is drawn from: the stage's drag and zoom, plus a slow spin.",
)
@node.function
def OrbitCamera(
    view: dict | None = None,
    spin: float = 0.0,
    tilt: float = 0.14,
    distance: float = 11.5,
) -> dict:
    """Yaw is the stage's drag plus `spin`; pitch is `tilt` plus the stage's drag; the stage's
    zoom scales `distance`. The burst count passes through, so the ball can tell a new burst."""
    view = view or {}
    yaw = float(view.get("yaw", 0.0)) + spin
    pitch = max(-1.2, min(1.2, tilt + float(view.get("pitch", 0.0))))
    zoom = max(0.4, min(2.5, float(view.get("zoom", 1.0))))
    return {
        "yaw": yaw,
        "pitch": pitch,
        "distance": distance * zoom,
        "burst": int(view.get("burst", 0)),
    }
