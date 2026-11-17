# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Function nodes whose pins are each typed by their signature: a Float, an Integer and a
Boolean in and out.

`Beat` turns stream time into a beat and `Warmth` reads the beat's count and accent, so a
typed Integer and a typed Boolean link between them as well as Floats.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

from tatolab.stream import Time, node

NANOSECONDS_PER_SECOND = 1_000_000_000

SECONDS_PER_MINUTE = 60.0


@dataclass(frozen=True)
class BeatReading:
    """Where a `Beat` is: how bright its glow is, which beat it is on and whether that beat is accented."""

    glow: float
    count: int
    accented: bool


@node.meta(
    category="Every pin kind",
    description="A glow that falls away after each beat, brighter on every accented beat.",
)
@node.function
def Beat(  # noqa: N802
    level: float = 0.5,
    beats_per_minute: float = 90.0,
    accent_every: int = 4,
    accent: bool = True,
    *,
    now: Time,
) -> BeatReading:
    """`level` falling to nothing across each beat at `beats_per_minute`, and half again as
    bright on every `accent_every`th beat while `accent` is on."""
    beats = now / NANOSECONDS_PER_SECOND * beats_per_minute / SECONDS_PER_MINUTE
    count = int(math.floor(beats))
    accented = accent and accent_every > 0 and count % accent_every == 0
    glow = level * (1.0 - (beats - count))
    if accented:
        glow = glow * 1.5
    return BeatReading(glow, count, accented)


@node.meta(
    category="Every pin kind",
    description="How warm the picture is: warmer on an accented beat, and a little warmer each beat of a bar.",
)
@node.function
def Warmth(  # noqa: N802
    count: int = 0, accented: bool = False, beats_per_bar: int = 4, base: float = 0.1
) -> float:
    """`base`, plus a step for each beat into the bar of `beats_per_bar`, doubled while `accented`."""
    beat_in_the_bar = count % beats_per_bar if beats_per_bar > 0 else 0
    warmth = base + 0.05 * beat_in_the_bar
    if accented:
        return warmth * 2.0
    return warmth
