# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`TextureInfo`: a picture's size, pixel format and frame rate, handed on as values."""

from __future__ import annotations

from typing import Annotated, Any, Callable, NamedTuple

from tatolab.stream import (
    Description,
    InputPin,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    Vector2,
    node,
)
from tatolab.stream.stream_facts import ImageStreamFacts


class _TextureInfoValues(NamedTuple):
    """The values `TextureInfo` hands on for one picture, the frame rate `None` while unstated."""

    width: int
    height: int
    pixel_format: str
    frame_rate: "float | None"


@node.meta(
    category="Math",
    description="Read a picture's width, height and size, its pixel format, and its frame rate when the picture states one, as values to wire into another node.",
)
@node(execution="reactive")
def TextureInfo(
    *,
    picture: InputPin[Annotated[Texture, StreamDelivery("newest")]],
    width: OutputPin[Annotated[int, Description("The picture's width in pixels")]],
    height: OutputPin[Annotated[int, Description("The picture's height in pixels")]],
    size: OutputPin[Annotated[Vector2, Description("The picture's width and height in pixels")]],
    pixel_format: OutputPin[
        Annotated[str, Description("The picture's texture format, such as rgba8_unorm")]
    ],
    frame_rate: OutputPin[
        Annotated[
            float,
            Description("Frames a second, written only when the stream feeding the picture states it"),
        ]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Hands on the values of each picture it reads. It writes an output only when its value
    has changed since it was last written; each output flows as a value, so the runtime sends
    its last write to a reader that connected after it."""
    values_written: "_TextureInfoValues | None" = None

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal values_written
        frame = picture.read()
        if frame is None:
            return
        values = _TextureInfoValues(
            frame.width, frame.height, frame.format, _frame_rate_the_stream_states(picture)
        )
        written = values_written

        if values.width != (written and written.width):
            width.write(values.width)
        if values.height != (written and written.height):
            height.write(values.height)
        if values[:2] != (written and written[:2]):
            size.write(Vector2(float(values.width), float(values.height)))
        if values.pixel_format != (written and written.pixel_format):
            pixel_format.write(values.pixel_format)
        if values.frame_rate is not None and values.frame_rate != (written and written.frame_rate):
            frame_rate.write(values.frame_rate)
        values_written = values

    return each_frame


def _frame_rate_the_stream_states(picture: Any) -> "float | None":
    stream_facts = picture.upstream_stream_facts()
    if not isinstance(stream_facts, ImageStreamFacts) or stream_facts.frame_rate is None:
        return None
    rate = stream_facts.frame_rate
    return rate.numerator / rate.denominator if rate.denominator else None
