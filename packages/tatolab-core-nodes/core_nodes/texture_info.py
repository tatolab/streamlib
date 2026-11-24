# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`TextureInfo`: a picture's size, pixel format and frame rate, handed on as values."""

from __future__ import annotations

from typing import Annotated, NamedTuple

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


class TextureFacts(NamedTuple):
    """What `TextureInfo` last handed on: a picture's size, its pixel format and its frame rate."""

    width: int
    height: int
    pixel_format: str
    frame_rate: "float | None"


@node.meta(
    category="Math",
    description="Read a picture's width, height and size, its pixel format, and its frame rate when the picture states one, as values to wire into another node.",
)
@node
class TextureInfo:
    """Hands on the facts of each picture it reads, writing an output only when its value has
    changed since it was last written."""

    picture: InputPin[Annotated[Texture, StreamDelivery("newest")]]
    width: OutputPin[Annotated[int, Description("The picture's width in pixels")]]
    height: OutputPin[Annotated[int, Description("The picture's height in pixels")]]
    size: OutputPin[Annotated[Vector2, Description("The picture's width and height in pixels")]]
    pixel_format: OutputPin[
        Annotated[str, Description("The picture's texture format, such as rgba8_unorm")]
    ]
    frame_rate: OutputPin[
        Annotated[
            float,
            Description("Frames a second, written only when the stream feeding the picture states it"),
        ]
    ]

    def __init__(self) -> None:
        self._facts_written: "TextureFacts | None" = None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        del ctx
        frame = self.picture.read()
        if frame is None:
            return
        facts = TextureFacts(
            frame.width, frame.height, frame.format, self._frame_rate_the_stream_states()
        )
        written = self._facts_written
        if written is None or (facts.width, facts.height) != (written.width, written.height):
            self.width.write(facts.width)
            self.height.write(facts.height)
            self.size.write(Vector2(float(facts.width), float(facts.height)))
        if written is None or facts.pixel_format != written.pixel_format:
            self.pixel_format.write(facts.pixel_format)
        if facts.frame_rate is not None and (written is None or facts.frame_rate != written.frame_rate):
            self.frame_rate.write(facts.frame_rate)
        self._facts_written = facts

    def _frame_rate_the_stream_states(self) -> "float | None":
        stream_facts = self.picture.upstream_stream_facts()
        if not isinstance(stream_facts, ImageStreamFacts) or stream_facts.frame_rate is None:
            return None
        rate = stream_facts.frame_rate
        return rate.numerator / rate.denominator if rate.denominator else None
