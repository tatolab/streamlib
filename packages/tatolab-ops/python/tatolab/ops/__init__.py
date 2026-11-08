# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The small function nodes every stream can wire, each a `@node.function` compiled to native code.

Each runs inside the read of the node it feeds, with no process of its own.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

from tatolab.stream import State, Stateful, Time, node

__all__ = ["Clamp", "Lfo", "LfoPhase", "Remap"]


@dataclass(frozen=True)
class LfoPhase:
    """Where an `Lfo` is in its cycle, and the stream time it was last evaluated at."""

    cycles: float = 0.0
    last_evaluated_at_ns: int = 0
    started: bool = False


@node.function
def Lfo(  # noqa: N802
    rate_hz: float = 1.0, *, now: Time, previous: State[LfoPhase]
) -> Stateful[float, LfoPhase]:
    """A sine wave from -1 to 1 at `rate_hz`, advanced by the stream time between reads."""
    elapsed_seconds = (now - previous.last_evaluated_at_ns) * 1e-9 if previous.started else 0.0
    cycles = (previous.cycles + rate_hz * elapsed_seconds) % 1.0
    return math.sin(2.0 * math.pi * cycles), LfoPhase(cycles, now, True)


@node.function
def Remap(  # noqa: N802
    x: float,
    from_low: float = -1.0,
    from_high: float = 1.0,
    to_low: float = 0.0,
    to_high: float = 1.0,
) -> float:
    """`x` mapped linearly from `from_low`..`from_high` onto `to_low`..`to_high`."""
    if from_high == from_low:
        return to_low
    return to_low + (x - from_low) * (to_high - to_low) / (from_high - from_low)


@node.function
def Clamp(x: float, low: float = 0.0, high: float = 1.0) -> float:  # noqa: N802
    """`x` held between `low` and `high`."""
    return min(max(x, low), high)
