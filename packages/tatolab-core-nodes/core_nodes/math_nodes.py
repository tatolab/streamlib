# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Number nodes: arithmetic, ranges and noise."""

from __future__ import annotations

import math

from tatolab.stream import Time, node

# The low 32 bits, the width every hash below works in so a compiled body and
# the same body run as Python agree bit for bit.
_LOW_32_BITS = 0xFFFFFFFF


@node.function(
    description="Rescale a number from one range to another. By default -1..1 becomes 0..1, which fits an LFO to a 0..1 input.",
    category="Math",
)
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


@node.function(
    description="Hold a number between a low and a high limit.",
    category="Math",
)
def Clamp(x: float, low: float = 0.0, high: float = 1.0) -> float:  # noqa: N802
    """`x` held between `low` and `high`."""
    return min(max(x, low), high)


@node.function(
    description="Add two numbers.",
    category="Math",
)
def Add(a: float = 0.0, b: float = 0.0) -> float:  # noqa: N802
    """`a` plus `b`."""
    return a + b


@node.function(
    description="Subtract b from a.",
    category="Math",
)
def Subtract(a: float = 0.0, b: float = 0.0) -> float:  # noqa: N802
    """`a` minus `b`."""
    return a - b


@node.function(
    description="Multiply two numbers.",
    category="Math",
)
def Multiply(a: float = 1.0, b: float = 1.0) -> float:  # noqa: N802
    """`a` times `b`."""
    return a * b


@node.function(
    description="Divide a by b. Gives 0 when b is 0.",
    category="Math",
)
def Divide(a: float = 0.0, b: float = 1.0) -> float:  # noqa: N802
    """`a` divided by `b`, and 0 when `b` is 0."""
    if b == 0.0:
        return 0.0
    return a / b


@node.function(
    description="Raise a number to a power.",
    category="Math",
)
def Power(base: float = 0.0, exponent: float = 1.0) -> float:  # noqa: N802
    """`base` raised to `exponent`, and 0 where that is no real number."""
    if base < 0.0 and exponent != math.floor(exponent):
        return 0.0
    if base == 0.0 and exponent < 0.0:
        return 0.0
    return base**exponent


@node.function(
    description="The smaller of two numbers.",
    category="Math",
)
def Minimum(a: float = 0.0, b: float = 0.0) -> float:  # noqa: N802
    """The smaller of `a` and `b`."""
    return min(a, b)


@node.function(
    description="The larger of two numbers.",
    category="Math",
)
def Maximum(a: float = 0.0, b: float = 0.0) -> float:  # noqa: N802
    """The larger of `a` and `b`."""
    return max(a, b)


@node.function(
    description="Drop a number's sign.",
    category="Math",
)
def Absolute(x: float = 0.0) -> float:  # noqa: N802
    """`x` without its sign."""
    return abs(x)


@node.function(
    description="The remainder after dividing a by b. Wraps a rising number into a repeating range.",
    category="Math",
)
def Modulo(a: float = 0.0, b: float = 1.0) -> float:  # noqa: N802
    """What is left of `a` after taking out whole `b`s, with the sign of `b`, and 0 when `b` is 0."""
    if b == 0.0:
        return 0.0
    return a - b * math.floor(a / b)


@node.function(
    description="Blend between two numbers: a at t = 0, b at t = 1.",
    category="Math",
)
def Lerp(a: float = 0.0, b: float = 1.0, t: float = 0.5) -> float:  # noqa: N802
    """The value `t` of the way from `a` to `b`: `a` at 0, `b` at 1."""
    return a + (b - a) * t


@node.function(
    description="Ease from 0 to 1 as a number crosses from one edge to the other.",
    category="Math",
)
def Smoothstep(x: float, edge_low: float = 0.0, edge_high: float = 1.0) -> float:  # noqa: N802
    """0 at or below `edge_low`, 1 at or above `edge_high`, and an S-curve between."""
    if edge_high == edge_low:
        return 0.0 if x < edge_low else 1.0
    t = min(max((x - edge_low) / (edge_high - edge_low), 0.0), 1.0)
    return t * t * (3.0 - 2.0 * t)


@node.function(
    description="0 below an edge, 1 at or above it.",
    category="Math",
)
def Step(x: float, edge: float = 0.0) -> float:  # noqa: N802
    """0 below `edge` and 1 at or above it."""
    return 0.0 if x < edge else 1.0


@node.function(
    description="A number you set once and wire to many inputs.",
    category="Math",
)
def Constant(value: float = 0.0) -> float:  # noqa: N802
    """`value`, unchanged: a number set once on its pin and read by many."""
    return value


@node.function(
    description="A new random number every frame, between low and high.",
    category="Math",
)
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


@node.function(
    description="Smooth wandering noise between -1 and 1. Wire a Timer into x to make it drift.",
    category="Math",
)
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
