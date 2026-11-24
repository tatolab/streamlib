# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The bundles Paint Pulse's nodes pass, and the function nodes that turn them into pins.

`MotionReadout` unpacks the listener's `Motion` into Float pins, so built-ins such as Lag,
Remap, RgbSplit and HsvAdjust can follow the music. `OrbitCamera` folds the stage view's drag
and zoom into one `BallCamera`, adding the slow spin a Speed node accumulates.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated, NamedTuple

from tatolab.stream import Description, node

BAND_COUNT = 24


@dataclass(frozen=True)
class Motion:
    """The music as the ball moves to it, one per listening window: how far each of the 24
    bands has flown out (0 to 3) and how loud each is (0 to 1), the kick's pump (1 on a hit and
    falling away), the overall energy, and the levels of the lowest six and highest eight bands."""

    clocks: list[float]
    levels: list[float]
    beat: float
    energy: float
    bass: float
    highs: float


#: The music before the listener has heard any.
SILENCE = Motion([0.0] * BAND_COUNT, [0.0] * BAND_COUNT, 0.0, 0.0, 0.0, 0.0)


@dataclass(frozen=True)
class StageCamera:
    """Where the stage view has been dragged and zoomed to, and how many bursts it has asked for."""

    yaw: float = 0.0
    pitch: float = 0.0
    zoom: float = 1.0
    burst: int = 0


#: The stage before it has been dragged, zoomed or clicked.
STAGE_AT_REST = StageCamera()


@dataclass(frozen=True)
class BallCamera:
    """Where the ball is drawn from, and how many bursts the stage has asked for."""

    yaw: float = 0.0
    pitch: float = 0.14
    distance: float = 11.5
    burst: int = 0


class MotionReading(NamedTuple):
    """The music as numbers: the kick's pump, the overall energy, and the bass and highs levels."""

    beat: float
    energy: float
    bass: float
    highs: float


@node.meta(
    category="Paint Pulse",
    description="Unpack the listener's motion into Float pins: beat, energy, bass and highs.",
)
@node
def MotionReadout(
    motion: Annotated[Motion, Description("From SpectrumListener")] = SILENCE,
) -> MotionReading:
    """Each named number of `motion`, or 0 before the listener has heard any."""
    return MotionReading(motion.beat, motion.energy, motion.bass, motion.highs)


@node.meta(
    category="Paint Pulse",
    description="The camera the ball is drawn from: the stage's drag and zoom, plus a slow spin.",
)
@node
def OrbitCamera(
    view: Annotated[StageCamera, Description("From the stage view")] = STAGE_AT_REST,
    spin: Annotated[float, Description("Radians added to the stage's yaw.")] = 0.0,
    tilt: Annotated[float, Description("Radians added to the stage's pitch.")] = 0.14,
    distance: Annotated[float, Description("How far the camera sits at the stage's own zoom.")] = 11.5,
) -> BallCamera:
    """Yaw is the stage's drag plus `spin`; pitch is `tilt` plus the stage's drag; the stage's
    zoom scales `distance`. The burst count passes through, so the ball can tell a new burst."""
    return BallCamera(
        yaw=view.yaw + spin,
        pitch=max(-1.2, min(1.2, tilt + view.pitch)),
        distance=distance * max(0.4, min(2.5, view.zoom)),
        burst=view.burst,
    )
