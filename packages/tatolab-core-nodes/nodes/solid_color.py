# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`SolidColor`: one flat colour at a set size, the picture a generator takes its size from."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from tatolab.stream import ColorInfo, Image, ImageFrameToWrite, NodeOutputTextureRing, Output, node

#: The texture format a `SolidColor` publishes.
SOLID_COLOR_TEXTURE_FORMAT = "rgba8_unorm"

#: Two slots, so a frame published on a config change never waits on a reader
#: still holding the one before it.
SOLID_COLOR_OUTPUT_RING_DEPTH = 2

#: The colour space the config's channels are read in: sRGB, full range.
SOLID_COLOR_COLOUR = ColorInfo(primaries="bt709", transfer="srgb", matrix="identity", range="full")


@dataclass(frozen=True)
class SolidColorConfig:
    """The size of a `SolidColor`'s picture and its colour, each channel from 0 to 1."""

    width: int = 1920
    height: int = 1080
    red: float = 0.0
    green: float = 0.0
    blue: float = 0.0
    alpha: float = 1.0


def rgba_bytes_of_the_solid_color(config: SolidColorConfig) -> tuple[int, int, int, int]:
    """`config`'s colour as four 8-bit channels, each held between 0 and 255."""
    return tuple(  # type: ignore[return-value]
        round(min(max(channel, 0.0), 1.0) * 255.0)
        for channel in (config.red, config.green, config.blue, config.alpha)
    )


@node.process(
    execution="continuous",
    interval_ms=100,
    description="A flat colour at the size you set. Also the blank canvas a generator such as Noise or Gradient takes its size from.",
    category="Generators",
)
class SolidColor:
    """Publishes one picture of its config's colour and size, once at start and again on each config change.

    A reader that connects after a publish is sent the same picture again.
    """

    image = Output(Image, formats=(SOLID_COLOR_TEXTURE_FORMAT,))

    def __init__(self, config: SolidColorConfig) -> None:
        self._config = config
        self._output_texture_ring = NodeOutputTextureRing(
            SOLID_COLOR_TEXTURE_FORMAT, ["texture_binding"], depth=SOLID_COLOR_OUTPUT_RING_DEPTH
        )
        self._publish_on_the_next_process = True
        self._frame_of_the_last_publish: Any = None

    def configure(self, config: SolidColorConfig) -> None:
        """Publish `config`'s colour and size on the next process."""
        self._config = config
        self._publish_on_the_next_process = True

    def process(self, ctx) -> None:
        if not self._publish_on_the_next_process:
            if self._frame_of_the_last_publish is not None and ctx.outputs.has_a_reader_the_last_write_missed(
                self.image
            ):
                ctx.outputs.write(self.image, self._frame_of_the_last_publish)
            return
        width, height = self._config.width, self._config.height
        if width < 1 or height < 1:
            raise ValueError(
                f"SolidColor's size is {width}x{height}. To fix: set `width` and `height` to 1 or more"
            )
        slot = self._output_texture_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, width, height
        )
        slot.lock(read_only=False)
        try:
            slot.as_numpy()[:height, :width] = rgba_bytes_of_the_solid_color(self._config)
        finally:
            slot.unlock()
        frame = ImageFrameToWrite.of_texture(slot, SOLID_COLOR_COLOUR)
        ctx.outputs.write(self.image, frame)
        self._frame_of_the_last_publish = frame
        self._publish_on_the_next_process = False
