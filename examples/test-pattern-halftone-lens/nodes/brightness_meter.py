# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Logic on the CPU: reads the effect's frames on the host and logs a number.

Importable as `nodes.brightness_meter:BrightnessMeter`. It is a sink off a
fan-out in its own child interpreter, so nothing it does can slow the picture —
swap the mean for a call to a model server and the app keeps its frame rate.
"""

from typing import Annotated, Callable

import numpy
from tatolab.stream import (
    Description,
    InputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    log,
    node,
)

BRIGHTNESS_REPORT_INTERVAL_NS = 1_000_000_000


@node(execution="reactive")
def BrightnessMeter(
    *,
    video_from_upstream: InputPin[
        Annotated[Texture, StreamDelivery("newest"), Description("Frames to measure")]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Logs the mean brightness of the frames it sees, once a second."""

    next_brightness_report_at_ns = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal next_brightness_report_at_ns
        frame = video_from_upstream.read()
        # `ctx.time` is stream time in ns, on the clock the stream runs on.
        if frame is None or ctx.time < next_brightness_report_at_ns:
            return
        # `frame.cpu()` is the slow door, named so: the frame lives on the GPU
        # and this reads it back into host memory. Once a second is cheap;
        # per-frame pixel work belongs in a GPU effect like HalftoneLensEffect.
        with frame.cpu() as pixels:
            # Color channels only — alpha is opaque and would skew the mean.
            brightness = float(numpy.mean(pixels[:, :, :3]))
        log.info(
            "brightness",
            mean=round(brightness, 1),
            width=frame.width,
            height=frame.height,
        )
        next_brightness_report_at_ns = ctx.time + BRIGHTNESS_REPORT_INTERVAL_NS

    return each_frame
