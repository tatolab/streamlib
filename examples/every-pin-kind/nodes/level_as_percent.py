# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`LevelAsPercent`, the sample's own conversion from a Float into a String.

It states that it converts, so a Graph Input saving a Float that is retyped to a String saves
the text this body gives for its value: 0.1 becomes `10%`. Its body computes from its input
alone, so the runtime runs it once to cast a saved value, with no stream started.

That cast runs this Python body, even where the stream runs it compiled with Numba, and with
the conversion's default config, since no node of it is placed when the Graph Input is
retyped.
"""

from __future__ import annotations

from typing import Annotated

from tatolab.stream import Description, node


@node.meta(
    category="Every pin kind",
    description="A level from 0 to 1 as the percentage it reads as.",
    conversion=True,
)
@node
def LevelAsPercent(
    level: Annotated[float, Description("The level, from 0 to 1.")] = 0.0,
) -> str:
    """`level` as a whole percentage, `0.1` as `10%`."""
    return f"{round(level * 100)}%"
