# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Number nodes that move with stream time."""

from __future__ import annotations

import math
from dataclasses import dataclass

from tatolab.stream import State, Stateful, Time, node


@dataclass(frozen=True)
class LfoPhase:
    """Where an `Lfo` is in its cycle, and the stream time it was last evaluated at."""

    cycles: float = 0.0
    last_evaluated_at_ns: int = 0
    started: bool = False


@node.meta(
    category="Animation",
    description="A smooth wave that swings between -1 and 1, rate_hz times a second. Wire it into any number input to animate it.",
)
@node.function
def Lfo(  # noqa: N802
    rate_hz: float = 1.0, *, now: Time, previous: State[LfoPhase]
) -> Stateful[float, LfoPhase]:
    """A sine wave from -1 to 1 at `rate_hz`, advanced by the stream time between reads."""
    elapsed_seconds = (now - previous.last_evaluated_at_ns) * 1e-9 if previous.started else 0.0
    cycles = (previous.cycles + rate_hz * elapsed_seconds) % 1.0
    return math.sin(2.0 * math.pi * cycles), LfoPhase(cycles, now, True)


@dataclass(frozen=True)
class TimerStart:
    """The stream time a `Timer` counts from."""

    started_at_ns: int = 0
    started: bool = False


@node.meta(
    category="Animation",
    description="Seconds since the stream started, or since reset was last on.",
)
@node.function
def Timer(  # noqa: N802
    reset: bool = False, *, now: Time, previous: State[TimerStart]
) -> Stateful[float, TimerStart]:
    """The seconds of stream time since the first evaluation, or since the last one `reset` was on."""
    if reset or not previous.started:
        return 0.0, TimerStart(now, True)
    return (now - previous.started_at_ns) * 1e-9, previous


@dataclass(frozen=True)
class LagValue:
    """Where a `Lag` has got to, and the stream time it got there."""

    value: float = 0.0
    last_evaluated_at_ns: int = 0
    started: bool = False


@node.meta(
    category="Animation",
    description="Smooth a jumpy number: follow it, taking about `seconds` to catch up.",
)
@node.function
def Lag(  # noqa: N802
    x: float, seconds: float = 0.1, *, now: Time, previous: State[LagValue]
) -> Stateful[float, LagValue]:
    """`x` followed smoothly, closing about two thirds of the gap each `seconds` of stream time."""
    if not previous.started or seconds <= 0.0:
        return x, LagValue(x, now, True)
    elapsed_seconds = (now - previous.last_evaluated_at_ns) * 1e-9
    followed = x + (previous.value - x) * math.exp(-elapsed_seconds / seconds)
    return followed, LagValue(followed, now, True)


@dataclass(frozen=True)
class RampPhase:
    """Where a `Ramp` is in its cycle, and the stream time it was last evaluated at."""

    cycles: float = 0.0
    last_evaluated_at_ns: int = 0
    started: bool = False


@node.meta(
    category="Animation",
    description="A number that climbs from 0 to 1 and starts over, rate_hz times a second. Drives anything that loops: a wipe, a rotation, a scroll.",
)
@node.function
def Ramp(  # noqa: N802
    rate_hz: float = 1.0, *, now: Time, previous: State[RampPhase]
) -> Stateful[float, RampPhase]:
    """A sawtooth from 0 up to 1 at `rate_hz`, advanced by the stream time between reads."""
    elapsed_seconds = (now - previous.last_evaluated_at_ns) * 1e-9 if previous.started else 0.0
    cycles = (previous.cycles + rate_hz * elapsed_seconds) % 1.0
    return cycles, RampPhase(cycles, now, True)


@dataclass(frozen=True)
class PulsePhase:
    """Where a `Pulse` is in its cycle, and the stream time it was last evaluated at."""

    cycles: float = 0.0
    last_evaluated_at_ns: int = 0
    started: bool = False


@node.meta(
    category="Animation",
    description="On and off in a steady beat: on for the first `width` of each cycle, rate_hz times a second. Set rate_hz to BPM / 60 for a beat.",
)
@node.function
def Pulse(  # noqa: N802
    rate_hz: float = 2.0, width: float = 0.5, *, now: Time, previous: State[PulsePhase]
) -> Stateful[bool, PulsePhase]:
    """On while the cycle at `rate_hz` is in its first `width`, advanced by the stream time between reads."""
    elapsed_seconds = (now - previous.last_evaluated_at_ns) * 1e-9 if previous.started else 0.0
    cycles = (previous.cycles + rate_hz * elapsed_seconds) % 1.0
    return cycles < width, PulsePhase(cycles, now, True)


@dataclass(frozen=True)
class SpeedPosition:
    """How far a `Speed` has travelled, and the stream time it got there."""

    position: float = 0.0
    last_evaluated_at_ns: int = 0
    started: bool = False


@node.meta(
    category="Animation",
    description="Turn a speed into a position: adds `rate` every second, so a steady input makes a steadily growing number. Reset sends it back to 0.",
)
@node.function
def Speed(  # noqa: N802
    rate: float = 1.0, reset: bool = False, *, now: Time, previous: State[SpeedPosition]
) -> Stateful[float, SpeedPosition]:
    """`rate` per second summed over stream time, back to 0 while `reset` is on."""
    if reset or not previous.started:
        return 0.0, SpeedPosition(0.0, now, True)
    position = previous.position + rate * (now - previous.last_evaluated_at_ns) * 1e-9
    return position, SpeedPosition(position, now, True)


@dataclass(frozen=True)
class EnvelopeLevel:
    """Where an `Envelope` has got to, and the stream time it got there."""

    level: float = 0.0
    last_evaluated_at_ns: int = 0
    started: bool = False


@node.meta(
    category="Animation",
    description="Fade up while the gate is on and fade down when it lets go: rises to 1 over `attack_seconds`, falls to 0 over `release_seconds`.",
)
@node.function
def Envelope(  # noqa: N802
    gate: bool = False,
    attack_seconds: float = 0.1,
    release_seconds: float = 0.5,
    *,
    now: Time,
    previous: State[EnvelopeLevel],
) -> Stateful[float, EnvelopeLevel]:
    """A level rising linearly to 1 while `gate` is on and falling linearly to 0 while it is off."""
    elapsed_seconds = (now - previous.last_evaluated_at_ns) * 1e-9 if previous.started else 0.0
    if gate:
        level = 1.0 if attack_seconds <= 0.0 else min(previous.level + elapsed_seconds / attack_seconds, 1.0)
    else:
        level = 0.0 if release_seconds <= 0.0 else max(previous.level - elapsed_seconds / release_seconds, 0.0)
    return level, EnvelopeLevel(level, now, True)
