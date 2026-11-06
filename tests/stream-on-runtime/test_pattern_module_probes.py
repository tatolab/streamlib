# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probe that reads the test pattern module's frames from a processor interpreter.

Reports the first two Image frames it receives as one JSON marker line on
`tatolabd`'s standard error — the boundary under test is one processor's
device frames reaching a Python processor in an interpreter of its own.
"""

import dataclasses
import json

from tatolab.stream import Image, Input, log, node

RESULT_MARKER = "MARKER:FRAMES_SEEN "


@node
class ImageFrameProbe:
    """Reports the first two Image frames the test pattern publishes."""

    video_from_upstream = Input(Image, delivery="ordered")

    def __init__(self) -> None:
        self.frames_seen = []

    def process(self, ctx) -> None:
        if len(self.frames_seen) >= 2:
            return
        frame = ctx.inputs.read(self.video_from_upstream)
        if frame is None:
            return
        self.frames_seen.append(
            {
                "surface_id": frame.surface_id,
                "width": frame.width,
                "height": frame.height,
                "format": frame.format,
                "colour": dataclasses.asdict(frame.colour),
                "timestamp_ns": frame.timestamp_ns,
            }
        )
        if len(self.frames_seen) == 2:
            log.info(RESULT_MARKER + json.dumps(self.frames_seen))
