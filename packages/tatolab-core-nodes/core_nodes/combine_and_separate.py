# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Nodes that make a Color or a Vector from numbers, and split one into its numbers.

Their node and pin names are fixed: saved documents and the upgrade that merges saved numbers
into one Color or Vector name them. One pair of Vector nodes serves every size: `result` feeds
a Vector of two or three, which keeps its first components, and a smaller Vector into `vector`
reads each missing component as 0.
"""

from __future__ import annotations

from typing import NamedTuple

from tatolab.stream import Color, Vector4, node


class ColorComponents(NamedTuple):
    """A Color's red, green, blue and alpha."""

    red: float
    green: float
    blue: float
    alpha: float


class VectorComponents(NamedTuple):
    """A Vector's x, y, z and w."""

    x: float
    y: float
    z: float
    w: float


@node.meta(category="Color", description="Make a colour from red, green, blue and alpha numbers, each 0 to 1.")
@node
def CombineColor(  # noqa: N802
    red: float = 0.0, green: float = 0.0, blue: float = 0.0, alpha: float = 1.0
) -> Color:
    """The Color of `red`, `green`, `blue` and `alpha`."""
    return Color(red, green, blue, alpha)


@node.meta(category="Color", description="Split a colour into its red, green, blue and alpha numbers.")
@node
def SeparateColor(color: Color = Color(0.0, 0.0, 0.0, 1.0)) -> ColorComponents:  # noqa: N802
    """`color`'s red, green, blue and alpha."""
    return ColorComponents(color.red, color.green, color.blue, color.alpha)


@node.meta(category="Math", description="Make a Vector from x, y, z and w numbers. It feeds a Vector of any size.")
@node
def CombineVector(  # noqa: N802
    x: float = 0.0, y: float = 0.0, z: float = 0.0, w: float = 0.0
) -> Vector4:
    """The Vector of `x`, `y`, `z` and `w`."""
    return Vector4(x, y, z, w)


@node.meta(
    category="Math",
    description="Split a Vector into its x, y, z and w numbers. A smaller Vector gives 0 for what it lacks.",
)
@node
def SeparateVector(vector: Vector4 = Vector4(0.0, 0.0, 0.0, 0.0)) -> VectorComponents:  # noqa: N802
    """`vector`'s x, y, z and w."""
    return VectorComponents(vector.x, vector.y, vector.z, vector.w)
