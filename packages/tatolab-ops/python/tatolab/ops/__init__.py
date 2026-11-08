# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The small function nodes every stream can wire, each a `@node.function` compiled to native code.

Each runs inside the read of the node it feeds, with no process of its own.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

from tatolab.stream import State, Stateful, Time, UnreadMessage, node

__all__ = [
    "Absolute",
    "Add",
    "Clamp",
    "Compare",
    "Comparison",
    "Constant",
    "Counter",
    "CounterCount",
    "Divide",
    "Feedback",
    "Gate",
    "Lag",
    "LagValue",
    "Lerp",
    "Lfo",
    "LfoPhase",
    "Maximum",
    "Minimum",
    "Modulo",
    "Multiply",
    "PerlinNoise",
    "Power",
    "Random",
    "Remap",
    "SampleAndHold",
    "SampledValue",
    "Select",
    "Smoothstep",
    "Step",
    "Subtract",
    "Switch",
    "Timer",
    "TimerStart",
    "Trigger",
    "TriggerLevel",
]

# The low 32 bits, the width every hash below works in so a compiled body and
# the same body run as Python agree bit for bit.
_LOW_32_BITS = 0xFFFFFFFF


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


@node.function
def Feedback(value: float = 0.0) -> float:  # noqa: N802
    """`value` as it was at the last evaluation, starting at 0: the one node a loop of function nodes closes through."""
    return value


@node.function
def Add(a: float = 0.0, b: float = 0.0) -> float:  # noqa: N802
    """`a` plus `b`."""
    return a + b


@node.function
def Subtract(a: float = 0.0, b: float = 0.0) -> float:  # noqa: N802
    """`a` minus `b`."""
    return a - b


@node.function
def Multiply(a: float = 1.0, b: float = 1.0) -> float:  # noqa: N802
    """`a` times `b`."""
    return a * b


@node.function
def Divide(a: float = 0.0, b: float = 1.0) -> float:  # noqa: N802
    """`a` divided by `b`, and 0 when `b` is 0."""
    if b == 0.0:
        return 0.0
    return a / b


@node.function
def Power(base: float = 0.0, exponent: float = 1.0) -> float:  # noqa: N802
    """`base` raised to `exponent`, and 0 where that is no real number."""
    if base < 0.0 and exponent != math.floor(exponent):
        return 0.0
    if base == 0.0 and exponent < 0.0:
        return 0.0
    return base**exponent


@node.function
def Minimum(a: float = 0.0, b: float = 0.0) -> float:  # noqa: N802
    """The smaller of `a` and `b`."""
    return min(a, b)


@node.function
def Maximum(a: float = 0.0, b: float = 0.0) -> float:  # noqa: N802
    """The larger of `a` and `b`."""
    return max(a, b)


@node.function
def Absolute(x: float = 0.0) -> float:  # noqa: N802
    """`x` without its sign."""
    return abs(x)


@node.function
def Modulo(a: float = 0.0, b: float = 1.0) -> float:  # noqa: N802
    """What is left of `a` after taking out whole `b`s, with the sign of `b`, and 0 when `b` is 0."""
    if b == 0.0:
        return 0.0
    return a - b * math.floor(a / b)


@node.function
def Lerp(a: float = 0.0, b: float = 1.0, t: float = 0.5) -> float:  # noqa: N802
    """The value `t` of the way from `a` to `b`: `a` at 0, `b` at 1."""
    return a + (b - a) * t


@node.function
def Smoothstep(x: float, edge_low: float = 0.0, edge_high: float = 1.0) -> float:  # noqa: N802
    """0 at or below `edge_low`, 1 at or above `edge_high`, and an S-curve between."""
    if edge_high == edge_low:
        return 0.0 if x < edge_low else 1.0
    t = min(max((x - edge_low) / (edge_high - edge_low), 0.0), 1.0)
    return t * t * (3.0 - 2.0 * t)


@node.function
def Step(x: float, edge: float = 0.0) -> float:  # noqa: N802
    """0 below `edge` and 1 at or above it."""
    return 0.0 if x < edge else 1.0


@dataclass(frozen=True)
class Comparison:
    """How `a` compares with `b`, within a tolerance."""

    less: bool
    equal: bool
    greater: bool


@node.function
def Compare(a: float = 0.0, b: float = 0.0, tolerance: float = 0.0) -> Comparison:  # noqa: N802
    """Whether `a` is less than, equal to (within `tolerance`) or greater than `b`."""
    equal = abs(a - b) <= tolerance
    return Comparison(less=not equal and a < b, equal=equal, greater=not equal and a > b)


@node.function
def Gate(value: UnreadMessage, open: bool = True) -> UnreadMessage | None:  # noqa: N802, A002
    """`value` handed on unread while `open`, else nothing, of any family."""
    return value if open else None


@node.function
def Switch(on: bool = False, when_on: float = 1.0, when_off: float = 0.0) -> float:  # noqa: N802
    """`when_on` while `on`, else `when_off`."""
    return when_on if on else when_off


@node.function
def Select(  # noqa: N802
    index: int = 0,
    option_0: float = 0.0,
    option_1: float = 0.0,
    option_2: float = 0.0,
    option_3: float = 0.0,
) -> float:
    """The option `index` names, `index` held between 0 and 3."""
    held_index = min(max(index, 0), 3)
    if held_index == 0:
        return option_0
    if held_index == 1:
        return option_1
    if held_index == 2:
        return option_2
    return option_3


@node.function
def Constant(value: float = 0.0) -> float:  # noqa: N802
    """`value`, unchanged: a number set once on its pin and read by many."""
    return value


@node.function
def Random(  # noqa: N802
    low: float = 0.0, high: float = 1.0, seed: int = 0, *, now: Time
) -> float:
    """A value between `low` and `high` drawn afresh for each stream time, the same for the same `seed` and time."""
    hashed = (
        (now & _LOW_32_BITS)
        ^ ((((now >> 32) & _LOW_32_BITS) * 0x45D9F3B) & _LOW_32_BITS)
        ^ (((seed & _LOW_32_BITS) * 0x27D4EB2D) & _LOW_32_BITS)
    )
    hashed = (((hashed >> 16) ^ hashed) * 0x45D9F3B) & _LOW_32_BITS
    hashed = (((hashed >> 16) ^ hashed) * 0x45D9F3B) & _LOW_32_BITS
    hashed = (hashed >> 16) ^ hashed
    return low + (high - low) * (hashed / 4294967296.0)


@node.function
def PerlinNoise(x: float = 0.0, seed: int = 0) -> float:  # noqa: N802
    """Smooth noise along `x`, between -1 and 1 and 0 at each whole number, the same for the same `seed`."""
    left_lattice_point = int(math.floor(x))
    offset = x - left_lattice_point
    from_the_left = 0.0
    from_the_right = 0.0
    for side in range(2):
        hashed = ((left_lattice_point + side) & _LOW_32_BITS) ^ (
            ((seed & _LOW_32_BITS) * 0x27D4EB2D) & _LOW_32_BITS
        )
        hashed = (((hashed >> 16) ^ hashed) * 0x45D9F3B) & _LOW_32_BITS
        hashed = (((hashed >> 16) ^ hashed) * 0x45D9F3B) & _LOW_32_BITS
        hashed = (hashed >> 16) ^ hashed
        gradient = hashed / 2147483648.0 - 1.0
        if side == 0:
            from_the_left = gradient * offset
        else:
            from_the_right = gradient * (offset - 1.0)
    fade = offset * offset * offset * (offset * (offset * 6.0 - 15.0) + 10.0)
    return min(max(2.0 * (from_the_left + (from_the_right - from_the_left) * fade), -1.0), 1.0)


@dataclass(frozen=True)
class TimerStart:
    """The stream time a `Timer` counts from."""

    started_at_ns: int = 0
    started: bool = False


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
class TriggerLevel:
    """Whether a `Trigger`'s input was on at its last evaluation."""

    was_on: bool = False


@node.function
def Trigger(  # noqa: N802
    x: bool = False, *, previous: State[TriggerLevel]
) -> Stateful[bool, TriggerLevel]:
    """On for the one evaluation at which `x` turns on, and off otherwise."""
    return x and not previous.was_on, TriggerLevel(x)


@dataclass(frozen=True)
class CounterCount:
    """A `Counter`'s count, and whether its `increment` was on at its last evaluation."""

    count: int = 0
    increment_was_on: bool = False


@node.function
def Counter(  # noqa: N802
    increment: bool = False, reset: bool = False, *, previous: State[CounterCount]
) -> Stateful[int, CounterCount]:
    """How many times `increment` has turned on, back to 0 while `reset` is on."""
    if reset:
        return 0, CounterCount(0, increment)
    count = previous.count + 1 if increment and not previous.increment_was_on else previous.count
    return count, CounterCount(count, increment)


@dataclass(frozen=True)
class SampledValue:
    """The value a `SampleAndHold` holds, and whether its `sample` was on at its last evaluation."""

    held: float = 0.0
    sample_was_on: bool = False


@node.function
def SampleAndHold(  # noqa: N802
    x: float = 0.0, sample: bool = False, *, previous: State[SampledValue]
) -> Stateful[float, SampledValue]:
    """`x` as it was when `sample` last turned on, starting at 0."""
    held = x if sample and not previous.sample_was_on else previous.held
    return held, SampledValue(held, sample)
