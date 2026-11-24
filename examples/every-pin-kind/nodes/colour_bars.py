# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Two nodes whose Texture pins each state one format: BGRA bars out, RGBA bars in.

`BgraColourBars` writes four bars of known colours as `bgra8_unorm` and `ColourBarsCheck` takes
only `rgba8_unorm`, so a link between them connects through the runtime's `TextureFormatConvert`.
`ColourBarsCheck` reads each bar's middle pixel back and raises on one that is not the colour
written.
"""

from __future__ import annotations

from typing import Annotated

from tatolab.stream import (
    ColorInfo,
    Description,
    TextureToWrite,
    InputPin,
    NodeOutputTextureRing,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    log,
    monotonic_now_ns,
    node,
)

#: The colour of each bar from left to right, as red, green, blue and alpha bytes. The last bar
#: has unequal red and blue, so a frame read with them swapped fails it as well as the first two.
COLOUR_BARS_RGBA = (
    (255, 0, 0, 255),
    (0, 255, 0, 255),
    (0, 0, 255, 255),
    (255, 128, 0, 255),
)

COLOUR_BARS_FRAME_COLOUR = ColorInfo(primaries="bt709", transfer="srgb", range="full")


def colour_bar_of_column(column: int, frame_width: int) -> int:
    """Which bar column `column` of a frame `frame_width` wide falls in."""
    return min(column * len(COLOUR_BARS_RGBA) // frame_width, len(COLOUR_BARS_RGBA) - 1)


def first_column_of_colour_bar(bar: int, frame_width: int) -> int:
    """The first column `colour_bar_of_column` puts in bar `bar` of a frame `frame_width` wide."""
    return -(-bar * frame_width // len(COLOUR_BARS_RGBA))


def middle_column_of_colour_bar(bar: int, frame_width: int) -> int:
    """The column in the middle of the columns `colour_bar_of_column` puts in bar `bar`."""
    last_column = first_column_of_colour_bar(bar + 1, frame_width) - 1
    return (first_column_of_colour_bar(bar, frame_width) + last_column) // 2


def refuse_a_width_narrower_than_the_colour_bars(frame_width: int) -> None:
    """Raise unless a frame `frame_width` wide gives each bar a column of its own."""
    if frame_width < len(COLOUR_BARS_RGBA):
        raise ValueError(
            f"a frame {frame_width} wide is narrower than its {len(COLOUR_BARS_RGBA)} colour bars"
        )


@node.meta(
    category="Every pin kind",
    description="Four bars of known colours, written as bgra8_unorm pictures.",
)
@node(execution="continuous", interval_ms=33)
class BgraColourBars:
    """The same four colour bars in every frame, each frame `bgra8_unorm`, at the size the
    fields hold from the next frame on."""

    width: Annotated[
        int,
        NumberRange(minimum=len(COLOUR_BARS_RGBA)),
        Description("Each frame's width in pixels, at least one per bar."),
    ] = 320
    height: Annotated[
        int, NumberRange(minimum=1), Description("Each frame's height in pixels.")
    ] = 180
    bars: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats("bgra8_unorm"),
            Description("The colour bars, as bgra8_unorm pictures"),
        ]
    ]

    def __init__(self) -> None:
        self._frame_ring = NodeOutputTextureRing("bgra8_unorm", ["texture_binding"])

    def process(self, runtime_context: RuntimeContextLimitedAccess) -> None:
        refuse_a_width_narrower_than_the_colour_bars(self.width)
        try:
            frame_texture = self._frame_ring.next_texture_for_this_frame(
                runtime_context.gpu_limited_access, self.width, self.height
            )
        except RuntimeError as refusal:
            log.warn("BgraColourBars dropped a frame", error=str(refusal))
            return
        frame_texture.lock(read_only=False)
        try:
            pixels = frame_texture.as_numpy()
            for column in range(self.width):
                red, green, blue, alpha = COLOUR_BARS_RGBA[
                    colour_bar_of_column(column, self.width)
                ]
                pixels[:, column] = (blue, green, red, alpha)
        finally:
            frame_texture.unlock()
        self.bars.write(
            TextureToWrite.of_texture(frame_texture, COLOUR_BARS_FRAME_COLOUR),
            timestamp_ns=monotonic_now_ns(),
        )


@node.meta(
    category="Every pin kind",
    description="Reads the colour bars back as rgba8_unorm pictures and raises on a bar that is not the colour written.",
)
@node
class ColourBarsCheck:
    """A check of the middle pixel of each colour bar: a wrong one raises, and the count of
    frames checked is logged every `report_every_frames` frames."""

    report_every_frames: Annotated[
        int, NumberRange(minimum=1), Description("How many frames between two reports.")
    ] = 30
    bars: InputPin[
        Annotated[
            Texture,
            TexturePixelFormats("rgba8_unorm"),
            StreamDelivery("newest"),
            Description("The colour bars, as rgba8_unorm pictures"),
        ]
    ]

    def __init__(self) -> None:
        self.frames_checked = 0

    def process(self, runtime_context: RuntimeContextLimitedAccess) -> None:
        frame = self.bars.read()
        if frame is None:
            return
        refuse_a_width_narrower_than_the_colour_bars(frame.width)
        with frame.cpu() as pixels:
            middle_row = frame.height // 2
            read_back = [
                tuple(int(channel) for channel in pixels[middle_row, column])
                for column in (
                    middle_column_of_colour_bar(bar, frame.width)
                    for bar in range(len(COLOUR_BARS_RGBA))
                )
            ]
        if read_back != list(COLOUR_BARS_RGBA):
            raise RuntimeError(
                f"colour bars read back wrong from a {frame.format} picture: "
                f"{read_back}, written {list(COLOUR_BARS_RGBA)}"
            )
        self.frames_checked += 1
        if self.frames_checked % max(1, self.report_every_frames) == 0:
            log.info(
                "colour bars read back right",
                format=frame.format,
                frames_checked=self.frames_checked,
            )
