# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Number nodes that read the take and the soundtrack, so the graph can wire the lens to them.

Each is a `@node.function`: the runtime evaluates it every step from the
numbers wired into it, and its outputs are pins. Wire a Timer into `seconds`
and the soundtrack's kick, the take's sections, where the sun sits in the
frame and where the horizon falls all become numbers any knob can follow.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

from tatolab.stream import node

from nodes.neon_world import TakeMoment, camera_basis, project_to_frame, smoothstep, stage_moment, sun_direction, take_moment_at
from nodes.soundtrack_analysis import soundtrack_curves


@dataclass(frozen=True)
class SoundtrackReading:
    """The soundtrack at one instant: how hard the kick hits and how loud the track is."""

    beat: float
    energy: float


@node.meta(
    category="Gulf Stream Neon",
    description="The soundtrack's kick envelope and loudness, both 0 to 1, at `seconds` into the track.",
)
@node.function
def SoundtrackBeat(seconds: float = 0.0) -> SoundtrackReading:  # noqa: N802
    """The kick and the loudness of the take's soundtrack at `seconds`."""
    beat, energy = soundtrack_curves().at(seconds)
    return SoundtrackReading(beat, energy)


@dataclass(frozen=True)
class TakeReading:
    """Where the take is: its sections as 0-to-1 levels, and where the lens should focus."""

    drop: float
    sunset: float
    reveal: float
    focus_distance: float


@node.meta(
    category="Gulf Stream Neon",
    description="The take's sections as levels: `drop` is up from 10 s to 20 s, `sunset` rises over the build, `reveal` holds the real map; plus the focus distance.",
)
@node.function
def TakeBeats(seconds: float = 0.0) -> TakeReading:  # noqa: N802
    """The take's drop, sunset and reveal levels at `seconds`, and the distance to focus at."""
    moment = take_moment_at(seconds)
    return TakeReading(moment.drop, smoothstep(7.5, 10.0, seconds), moment.reveal, moment.focus_distance)


@dataclass(frozen=True)
class ScreenPlacement:
    """Where the sun and the horizon fall in the frame, from 0 to 1, and how much of the sun shows."""

    sun_x: float
    sun_y: float
    sun_visible: float
    horizon_y: float


@node.meta(
    category="Gulf Stream Neon",
    description="Where the take's camera sees the sun and the horizon, as frame positions from 0 to 1, for god rays, flares and heat haze.",
)
@node.function
def SunOnScreen(  # noqa: N802
    seconds: float = 0.0, aspect: float = 0.5625, vertical_fov_degrees: float = 62.0
) -> ScreenPlacement:
    """The sun's and the horizon's place in the frame at `seconds`, for a frame of `aspect` and lens."""
    return _placement_of(take_moment_at(seconds, aspect), aspect, vertical_fov_degrees)


def _placement_of(moment: TakeMoment, aspect: float, vertical_fov_degrees: float) -> ScreenPlacement:
    """Where `moment`'s camera sees the sun and the horizon."""
    sun = sun_direction(moment.time_of_day)
    far = 500.0
    sun_point = (
        moment.camera_x + sun[0] * far,
        moment.camera_y + sun[1] * far,
        moment.camera_altitude + sun[2] * far,
    )
    on_frame = project_to_frame(moment, sun_point, aspect, vertical_fov_degrees)
    forward, _right, _up = camera_basis(moment.camera_yaw_degrees, moment.camera_pitch_degrees)
    flat = math.hypot(forward[0], forward[1]) or 1.0
    horizon_point = (
        moment.camera_x + forward[0] / flat * far,
        moment.camera_y + forward[1] / flat * far,
        0.0,
    )
    horizon = project_to_frame(moment, horizon_point, aspect, vertical_fov_degrees)
    horizon_y = horizon[1] if horizon is not None else -1.0
    if on_frame is None:
        return ScreenPlacement(0.5, -1.0, 0.0, horizon_y)
    x, y = on_frame
    inside = smoothstep(-0.35, 0.0, x) * (1.0 - smoothstep(1.0, 1.35, x)) * smoothstep(-0.35, 0.0, y) * (1.0 - smoothstep(1.0, 1.35, y))
    above_sea = smoothstep(-0.1, 0.02, sun[2])
    return ScreenPlacement(x, y, inside * above_sea * (1.0 - moment.reveal), horizon_y)


@node.meta(
    category="Gulf Stream Neon",
    description="Where the stage view's camera sees the sun and the horizon, as frame positions from 0 to 1, for god rays, flares and heat haze while you fly it.",
)
@node.function
def StageSunOnScreen(  # noqa: N802
    stage: dict, time_of_day: float = 0.35, aspect: float = 1.7778, vertical_fov_degrees: float = 50.0
) -> ScreenPlacement:
    """The sun's and the horizon's place in the frame for the stage's camera at `time_of_day`."""
    return _placement_of(stage_moment(stage, time_of_day, 0.0), aspect, vertical_fov_degrees)
