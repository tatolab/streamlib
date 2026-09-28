# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Logic on the CPU: reads the effect's frames on the host and logs a number.

Importable as `processors.brightness_meter:BrightnessMeter`. It is a sink off a
fan-out in its own child interpreter, so nothing it does can slow the picture —
swap the mean for a call to a model server and the app keeps its frame rate.
"""

import numpy
from streamlib import (
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,  # noqa: A004 — streamlib's port decorator
    log,
    processor,
)

BRIGHTNESS_REPORT_INTERVAL_NS = 1_000_000_000


@processor
class BrightnessMeter:
    """Logs the mean brightness of the frames it sees, once a second."""

    next_brightness_report_at_ns: int = 0

    @input(delivery_profile="newest")
    def video_from_upstream(self) -> VideoFrame: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        # `ctx.time` is the monotonic clock every processor shares, in ns.
        if frame is None or ctx.time < self.next_brightness_report_at_ns:
            return
        # `frame.cpu()` is the slow door, named so: the frame lives on the GPU
        # and this reads it back into host memory. Once a second is cheap;
        # per-frame pixel work belongs in a GPU effect like InvertingEffect.
        with frame.cpu() as pixels:
            # Color channels only — alpha is opaque and would skew the mean.
            brightness = float(numpy.mean(pixels[:, :, :3]))
        log.info(
            "brightness",
            mean=round(brightness, 1),
            width=frame.width,
            height=frame.height,
        )
        self.next_brightness_report_at_ns = ctx.time + BRIGHTNESS_REPORT_INTERVAL_NS
