# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Number nodes that read the take and the soundtrack, so the graph can wire the lens to them.

Each is a function node: the runtime evaluates it every step from the
numbers wired into it, and its outputs are pins. Wire a Timer into `seconds`
and the soundtrack's kick, the take's sections, where the sun sits in the
frame and where the horizon falls all become numbers any knob can follow.
"""

from __future__ import annotations

import dataclasses
import math
from dataclasses import dataclass
from typing import Annotated, NamedTuple

from tatolab.stream import Description, State, Stateful, Time, node

from nodes.neon_world import (
    STAGE_CAMERA_AT_HOME,
    STAGE_HOME,
    StageCamera,
    StageLookValues,
    TakeMoment,
    camera_basis,
    project_to_frame,
    smoothstep,
    stage_moment,
    sun_direction,
    take_moment_at,
)
from nodes.soundtrack_analysis import soundtrack_curves


class SoundtrackReading(NamedTuple):
    """The soundtrack at one instant: how hard the kick hits and how loud the track is."""

    beat: float
    energy: float


@node.meta(
    category="Gulf Stream Neon",
    description="The soundtrack's kick envelope and loudness, both 0 to 1, at `seconds` into the track.",
)
@node
def SoundtrackBeat(  # noqa: N802
    seconds: Annotated[float, Description("Seconds into the track.")] = 0.0,
) -> SoundtrackReading:
    """The kick and the loudness of the take's soundtrack at `seconds`."""
    beat, energy = soundtrack_curves().at(seconds)
    return SoundtrackReading(beat, energy)


class TakeReading(NamedTuple):
    """Where the take is: its sections as 0-to-1 levels, and where the lens should focus."""

    drop: float
    sunset: float
    reveal: float
    focus_distance: float


@node.meta(
    category="Gulf Stream Neon",
    description="The take's sections as levels: `drop` is up from 10 s to 20 s, `sunset` rises over the build, `reveal` holds the real map; plus the focus distance.",
)
@node
def TakeBeats(  # noqa: N802
    seconds: Annotated[float, Description("Seconds into the take.")] = 0.0,
) -> TakeReading:
    """The take's drop, sunset and reveal levels at `seconds`, and the distance to focus at."""
    moment = take_moment_at(seconds)
    return TakeReading(moment.drop, smoothstep(7.5, 10.0, seconds), moment.reveal, moment.focus_distance)


class ScreenPlacement(NamedTuple):
    """Where the sun and the horizon fall in the frame, from 0 to 1, and how much of the sun shows."""

    sun_x: float
    sun_y: float
    sun_visible: float
    horizon_y: float


@node.meta(
    category="Gulf Stream Neon",
    description="Where the take's camera sees the sun and the horizon, as frame positions from 0 to 1, for god rays, flares and heat haze.",
)
@node
def SunOnScreen(  # noqa: N802
    seconds: Annotated[float, Description("Seconds into the take.")] = 0.0,
    aspect: Annotated[float, Description("The frame's width over its height.")] = 0.5625,
    vertical_fov_degrees: Annotated[float, Description("The lens's vertical field of view.")] = 62.0,
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
@node
def StageSunOnScreen(  # noqa: N802
    stage: Annotated[StageCamera, Description("The stage view's camera.")] = STAGE_CAMERA_AT_HOME,
    time_of_day: Annotated[float, Description("0 golden hour to 1 night.")] = 0.35,
    aspect: Annotated[float, Description("The frame's width over its height.")] = 1.7778,
    vertical_fov_degrees: Annotated[float, Description("The lens's vertical field of view.")] = 50.0,
) -> ScreenPlacement:
    """The sun's and the horizon's place in the frame for the stage's camera at `time_of_day`."""
    return _placement_of(stage_moment(stage, time_of_day, 0.0), aspect, vertical_fov_degrees)


@node.meta(
    category="Gulf Stream Neon",
    description="The stage's look as one record: the real map to neon morph, the time of day, the skyline, the neon and the current, each already smoothed upstream.",
)
@node
def StageLook(  # noqa: N802
    morph: Annotated[float, Description("0 the real map to 1 neon.")] = 0.0,
    time_of_day: Annotated[float, Description("0 golden hour to 1 night.")] = 0.3,
    skyline_height: Annotated[float, Description("How tall the skyline stands.")] = 0.055,
    neon: Annotated[float, Description("How brightly the neon glows.")] = 1.0,
    current_speed: Annotated[float, Description("How fast the current's time-lapse runs.")] = 1.0,
) -> StageLookValues:
    """The five stage values as one record, for the renderer and the current."""
    return StageLookValues(morph, time_of_day, skyline_height, neon, current_speed)


@dataclass(frozen=True)
class FollowedCamera:
    """Where `SmoothCamera`'s camera is, and the stream time it last moved."""

    target_x: float = STAGE_HOME["target_x"]
    target_y: float = STAGE_HOME["target_y"]
    distance: float = STAGE_HOME["distance"]
    yaw_degrees: float = STAGE_HOME["yaw_degrees"]
    pitch_degrees: float = STAGE_HOME["pitch_degrees"]
    last_evaluated_at_ns: int = 0
    started: bool = False


CAMERA_KEYS = ("target_x", "target_y", "distance", "yaw_degrees", "pitch_degrees")


@node.meta(
    category="Gulf Stream Neon",
    description="Glides the stage's camera after the hand: an exponential follow of orbit, zoom and fly that is the same at any frame rate. Clicks and eddy drops pass straight through.",
)
@node
def SmoothCamera(  # noqa: N802
    stage: Annotated[StageCamera, Description("The stage view's camera.")] = STAGE_CAMERA_AT_HOME,
    seconds: Annotated[float, Description("The follow's time constant, in seconds.")] = 0.18,
    *,
    now: Time,
    previous: State[FollowedCamera],
) -> Stateful[StageCamera, FollowedCamera]:
    """The camera eased toward the stage's, with `seconds` as the time constant."""
    wanted = {key: float(getattr(stage, key)) for key in CAMERA_KEYS}
    if not previous.started or seconds <= 0.0:
        followed = wanted
    else:
        keep = math.exp(-max(now - previous.last_evaluated_at_ns, 0) * 1e-9 / seconds)
        followed = {key: wanted[key] + (getattr(previous, key) - wanted[key]) * keep for key in CAMERA_KEYS}
    return dataclasses.replace(stage, **followed), FollowedCamera(**followed, last_evaluated_at_ns=now, started=True)


@dataclass(frozen=True)
class GlideState:
    """Where a `Glide` is, how fast it is moving, and the stream time it last moved."""

    value: float = 0.0
    velocity: float = 0.0
    last_evaluated_at_ns: int = 0
    started: bool = False


@node.meta(
    category="Gulf Stream Neon",
    description="Glides after a number like a critically damped spring: no overshoot, no corners, the same at any frame rate. Put it between a slider and what it drives, so a few edits a second play out as a smooth 60 fps move.",
)
@node
def Glide(  # noqa: N802
    x: Annotated[float, Description("The number to glide after.")] = 0.0,
    seconds: Annotated[float, Description("About how long the glide takes to settle.")] = 0.2,
    *,
    now: Time,
    previous: State[GlideState],
) -> Stateful[float, GlideState]:
    """`x`, followed by a critically damped spring that settles in about `seconds`."""
    if not previous.started or seconds <= 0.0:
        return x, GlideState(x, 0.0, now, True)
    dt = max(now - previous.last_evaluated_at_ns, 0) * 1e-9
    omega = 2.0 / seconds
    offset = previous.value - x
    carry = (previous.velocity + omega * offset) * dt
    decay = math.exp(-omega * dt)
    value = x + (offset + carry) * decay
    velocity = (previous.velocity - omega * carry) * decay
    return value, GlideState(value, velocity, now, True)
