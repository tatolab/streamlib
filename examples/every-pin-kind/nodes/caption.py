# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A function node whose inputs each name the control the Studio draws for them.

Each input of `Caption` carries `PinControl` with one of the Studio's own names, so the sample
shows every named control beside the type's default: a checkbox, a dial for a whole number and
for a fraction, text over several lines, a code editor, segments of a Menu, a two-axis pad and
a file picker.
"""

from __future__ import annotations

from typing import Annotated, Literal

from tatolab.stream import (
    Asset,
    Description,
    NumberRange,
    NumberStep,
    PinControl,
    Vector2,
    node,
)


@node.meta(
    category="Every pin kind",
    description="A caption's words and how it is laid out, each set with the control it names.",
)
@node
def Caption(
    shown: Annotated[
        bool, PinControl("checkbox"), Description("Whether the caption shows.")
    ] = True,
    lines: Annotated[
        int,
        NumberRange(minimum=1, maximum=8),
        PinControl("dial"),
        Description("How many lines the caption may take."),
    ] = 2,
    size: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=2.0),
        NumberStep(0.01),
        PinControl("dial"),
        Description("How large the caption is, against the frame's height."),
    ] = 1.0,
    words: Annotated[
        str, PinControl("multiline"), Description("The caption's words.")
    ] = ("Every pin kind\nin one stream"),
    style: Annotated[
        str, PinControl("code"), Description("How the caption is drawn.")
    ] = ("weight: 600;\nshadow: soft;"),
    align: Annotated[
        Literal["left", "centre", "right"],
        PinControl("segmented"),
        Description("Which side of its box the caption keeps to."),
    ] = "centre",
    anchor: Annotated[
        Vector2,
        PinControl("xy_pad"),
        Description("Where the caption's box sits in the frame."),
    ] = Vector2(0.0, -0.5),
    font: Annotated[
        Asset | None,
        PinControl("file_picker"),
        Description("The font the caption is set in."),
    ] = None,
) -> str:
    """The caption's words as shown: nothing while not `shown`, else at most `lines` of
    `words`."""
    del size, style, align, anchor, font
    if not shown:
        return ""
    return "\n".join(words.splitlines()[:lines])
