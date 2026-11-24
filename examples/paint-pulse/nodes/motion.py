# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Function nodes that turn the listener's record and the stage's camera into pins.

`MotionReadout` unpacks the listener's motion record into Float pins, so built-ins such as
Lag, Remap, RgbSplit and HsvAdjust can follow the music. `OrbitCamera` folds the stage view's
drag and zoom into one camera record, adding the slow spin a Speed node accumulates.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated, NamedTuple

from tatolab.stream import Description, node

from nodes.spectrum_listener import SpectrumMotion


class MotionReading(NamedTuple):
    """The music as numbers: the kick's pump, the overall energy, and the bass and highs levels."""

    beat: float
    energy: float
    bass: float
    highs: float


@dataclass(frozen=True)
class StageCamera:
    """The stage view's drag and zoom, and how many bursts it has fired."""

    yaw: float
    pitch: float
    zoom: float
    burst: int


@dataclass(frozen=True)
class OrbitView:
    """Where the ball is drawn from, and the burst count the ball tells a new burst by."""

    yaw: float
    pitch: float
    distance: float
    burst: int


@node.meta(
    category="Paint Pulse",
    description="Unpack the listener's motion record into Float pins: beat, energy, bass and highs.",
)
@node
def MotionReadout(
    motion: Annotated[
        SpectrumMotion, Description("The listener's motion record.")
    ] = SpectrumMotion(clocks=[], levels=[], beat=0.0, energy=0.0, bass=0.0, highs=0.0),
) -> MotionReading:
    """Each named number of `motion`, all 0 before the first record arrives."""
    return MotionReading(motion.beat, motion.energy, motion.bass, motion.highs)


@node.meta(
    category="Paint Pulse",
    description="The camera the ball is drawn from: the stage's drag and zoom, plus a slow spin.",
)
@node
def OrbitCamera(
    view: Annotated[
        StageCamera, Description("The stage view's drag and zoom.")
    ] = StageCamera(yaw=0.0, pitch=0.0, zoom=1.0, burst=0),
    spin: Annotated[float, Description("The slow spin added to the yaw.")] = 0.0,
    tilt: Annotated[float, Description("The pitch before the stage's drag.")] = 0.14,
    distance: Annotated[
        float, Description("How far the camera sits before the stage's zoom.")
    ] = 11.5,
) -> OrbitView:
    """Yaw is the stage's drag plus `spin`; pitch is `tilt` plus the stage's drag; the stage's
    zoom scales `distance`. The burst count passes through, so the ball can tell a new burst."""
    pitch = max(-1.2, min(1.2, tilt + view.pitch))
    zoom = max(0.4, min(2.5, view.zoom))
    return OrbitView(
        yaw=view.yaw + spin,
        pitch=pitch,
        distance=distance * zoom,
        burst=view.burst,
    )
