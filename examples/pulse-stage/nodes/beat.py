# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The stage's timing, as function nodes: a beat at a tempo, a pulse that follows it smoothly,
and the cue the click track plays.

`Beat` turns the seconds a `Timer` counts into a pulse that falls away across each beat, the
beat's count and whether it opens a bar. `Follow` eases toward its target by a share of the
gap each evaluation; its `previous` is linked from a `PreviousValue` of its own result, so the
stream closes a loop of function nodes. `ClickCue` packs a beat into the record `ClickTrack`
reads.
"""

from __future__ import annotations

import math
from typing import Annotated, NamedTuple

from tatolab.stream import Description, NumberRange, NumberStep, node

SECONDS_PER_MINUTE = 60.0


class BeatReading(NamedTuple):
    """Where a `Beat` is: its pulse, which beat it is on and whether that beat opens a bar."""

    pulse: float
    count: int
    downbeat: bool


@node.meta(
    category="Pulse Stage",
    description="A pulse that jumps to 1 on each beat and falls away before the next, with the beat's count and whether it opens a bar.",
)
@node
def Beat(
    seconds: Annotated[float, Description("The stream's clock, in seconds.")] = 0.0,
    beats_per_minute: Annotated[
        float,
        NumberRange(minimum=30.0, maximum=240.0),
        NumberStep(1.0),
        Description("How fast the beat is."),
    ] = 120.0,
    beats_per_bar: Annotated[
        int,
        NumberRange(minimum=1, maximum=16),
        Description("How many beats make a bar."),
    ] = 4,
) -> BeatReading:
    """The pulse at `seconds` for `beats_per_minute`, falling from 1 to 0 across each beat."""
    beats = seconds * beats_per_minute / SECONDS_PER_MINUTE
    count = math.floor(beats)
    pulse = (1.0 - (beats - count)) ** 3
    downbeat = beats_per_bar > 0 and count % beats_per_bar == 0
    return BeatReading(pulse, count, downbeat)


@node.meta(
    category="Pulse Stage",
    description="Eases toward target by a share of the gap each time it is read. Link previous from a Previous Value of its own result.",
)
@node
def Follow(
    target: Annotated[float, Description("The value to ease toward.")] = 0.0,
    previous: Annotated[
        float, Description("Its own result at the last evaluation.")
    ] = 0.0,
    rate: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=1.0),
        NumberStep(0.01),
        Description("How much of the gap it closes each time: 1 jumps, 0 never moves."),
    ] = 0.35,
) -> float:
    """`previous` moved toward `target` by `rate` of the gap between them."""
    return previous + (target - previous) * rate


@node.meta(
    category="Pulse Stage",
    description="The beat as the record the click track plays: its count and whether it opens a bar.",
)
@node.function
def ClickCue(count: int = 0, downbeat: bool = False) -> dict:
    """`count` and `downbeat` as one record."""
    return {"count": count, "downbeat": downbeat}
