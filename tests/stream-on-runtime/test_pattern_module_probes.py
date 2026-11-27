# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Probe that reads the test pattern module's frames from a node interpreter.

Reports the first two Texture frames it receives as one JSON marker line on
`tatolabd`'s standard error — the boundary under test is one node's
device frames reaching a Python node in an interpreter of its own.
"""

import dataclasses
import json
from typing import Annotated, Callable

from tatolab.stream import InputPin, RuntimeContextLimitedAccess, StreamDelivery, Texture, log, node

RESULT_MARKER = "MARKER:FRAMES_SEEN "


@node(execution="reactive")
def ImageFrameProbe(
    *,
    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports the first two Texture frames the test pattern publishes."""

    frames_seen = []

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        if len(frames_seen) >= 2:
            return
        frame = video_from_upstream.read()
        if frame is None:
            return
        frames_seen.append(
            {
                "surface_id": frame.surface_id,
                "width": frame.width,
                "height": frame.height,
                "format": frame.format,
                "colour": dataclasses.asdict(frame.colour),
                "timestamp_ns": frame.timestamp_ns,
            }
        )
        if len(frames_seen) == 2:
            log.info(RESULT_MARKER + json.dumps(frames_seen))

    return each_frame
