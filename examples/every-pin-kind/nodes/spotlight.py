# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A function node whose pins are a Color, a Vector, a Rectangle and a Transform.

Each is declared in the typed grammar of bare `@node` with a value class of `tatolab.stream`,
so a value set on an input reaches the body as that class, and the Color it returns travels on
a link as its four parts.
"""

from __future__ import annotations

from typing import Annotated

from tatolab.stream import Color, Description, PinMeaning, Rectangle, Transform, Vector2, node

#: How much of the tint a spot outside its area keeps.
STRENGTH_OUTSIDE_THE_AREA = 0.25


@node.meta(
    category="Every pin kind",
    description="The tint a spot of the picture takes: full inside an area, dimmed outside it.",
)
@node
def Spotlight(
    tint: Annotated[Color, Description("The colour the spot takes inside the area.")] = Color(
        1.0, 0.6, 0.2
    ),
    spot: Annotated[Vector2, Description("Where the spot is, as a share of the frame.")] = Vector2(
        0.5, 0.5
    ),
    area: Annotated[
        Rectangle, Description("The part of the frame the tint is full in, as a share of it.")
    ] = Rectangle(0.25, 0.25, 0.5, 0.5),
    move: Annotated[Transform, Description("What moves the spot before it is placed.")] = (
        Transform.identity()
    ),
) -> Annotated[Color, PinMeaning("tint")]:
    """`tint` where `spot`, moved by `move`, falls inside `area`, and a quarter of it elsewhere."""
    numbers = move.numbers
    x = numbers[0] * spot.x + numbers[1] * spot.y + numbers[3]
    y = numbers[4] * spot.x + numbers[5] * spot.y + numbers[7]
    inside = area.x <= x <= area.x + area.width and area.y <= y <= area.y + area.height
    strength = 1.0 if inside else STRENGTH_OUTSIDE_THE_AREA
    return Color(tint.red * strength, tint.green * strength, tint.blue * strength, tint.alpha)
