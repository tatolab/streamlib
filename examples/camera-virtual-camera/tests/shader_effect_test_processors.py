# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""The two ends a `ShaderEffect` is tested between.

The source publishes a known pattern the way `CameraSource` publishes a frame —
a buffer-backed surface — so the effect's landing copy runs exactly as it does
behind a camera. The sink reports the rendered pixels at the coordinates it was
configured with, one line per frame, for the test to hold against a reference
computed on the CPU.
"""

import json
from dataclasses import dataclass, field

from streamlib import (
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    clock,
    input,  # noqa: A004 — streamlib's port decorator
    log,
    output,
    processor,
)

KNOWN_PATTERN_WIDTH = 64
# Not square, so a shader that swaps its axes cannot pass.
KNOWN_PATTERN_HEIGHT = 48

RENDERED_PIXELS_MARKER = "MARKER:RENDERED_PIXELS "


def known_pattern_pixel_at(x: int, y: int) -> "tuple[int, int, int, int]":
    """The RGBA8 value the source publishes at column `x`, row `y`."""
    return (4 * x, 5 * y, 96, 255)


@processor(
    execution="continuous",
    interval_ms=20,
    description="Publishes a known RGBA pattern as a buffer-backed frame",
)
class KnownPatternPixelBufferSource:
    """The pattern, filled once, published every tick."""

    @output()
    def video_to_downstream(self) -> None: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Held for the processor's life, so every id this publishes stays live.
        self.known_pattern_pixel_buffer = ctx.gpu_full_access.acquire_pixel_buffer(
            KNOWN_PATTERN_WIDTH, KNOWN_PATTERN_HEIGHT, "rgba"
        )
        self.known_pattern_pixel_buffer.lock(read_only=False)
        try:
            pixels = self.known_pattern_pixel_buffer.as_numpy()
            for y in range(KNOWN_PATTERN_HEIGHT):
                for x in range(KNOWN_PATTERN_WIDTH):
                    pixels[y, x] = known_pattern_pixel_at(x, y)
        finally:
            self.known_pattern_pixel_buffer.unlock()

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.outputs.write(
            "video_to_downstream",
            {
                "surface_id": self.known_pattern_pixel_buffer.surface_id,
                "width": KNOWN_PATTERN_WIDTH,
                "height": KNOWN_PATTERN_HEIGHT,
                "timestamp_ns": clock.monotonic_now_ns(),
            },
        )


@dataclass
class RenderedPixelReportingSinkConfig:
    """`[x, y]` pairs whose rendered value each frame's report carries."""

    pixel_coordinates_to_report: "list[list[int]]" = field(default_factory=list)


@processor(description="Reports rendered pixels at configured coordinates")
class RenderedPixelReportingSink:
    """One marker line per frame: the frame count and the requested pixels."""

    @input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    def __init__(self, config: RenderedPixelReportingSinkConfig) -> None:
        self.pixel_coordinates_to_report = config.pixel_coordinates_to_report
        self.frames_reported = 0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None:
            return
        with frame.cpu() as rendered_pixels:
            reported_pixels = [
                [int(channel) for channel in rendered_pixels[y, x]]
                for x, y in self.pixel_coordinates_to_report
            ]
        self.frames_reported += 1
        log.info(
            RENDERED_PIXELS_MARKER
            + json.dumps(
                {
                    "frame": self.frames_reported,
                    "extent": [frame.width, frame.height],
                    "pixels": reported_pixels,
                }
            )
        )
