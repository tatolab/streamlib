# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Probe that reads the test pattern module's frames from a node interpreter.

Reports the first two Texture frames it receives as one JSON marker line on
`tatolabd`'s standard error — the boundary under test is one node's
device frames reaching a Python node in an interpreter of its own.
"""

import dataclasses
import json
from typing import Annotated

from tatolab.stream import InputPin, StreamDelivery, Texture, log, node

RESULT_MARKER = "MARKER:FRAMES_SEEN "


@node
class ImageFrameProbe:
    """Reports the first two Texture frames the test pattern publishes."""

    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("ordered")]]

    def __init__(self) -> None:
        self._frames_seen = []

    def process(self, ctx) -> None:
        if len(self._frames_seen) >= 2:
            return
        frame = self.video_from_upstream.read()
        if frame is None:
            return
        self._frames_seen.append(
            {
                "surface_id": frame.surface_id,
                "width": frame.width,
                "height": frame.height,
                "format": frame.format,
                "colour": dataclasses.asdict(frame.colour),
                "timestamp_ns": frame.timestamp_ns,
            }
        )
        if len(self._frames_seen) == 2:
            log.info(RESULT_MARKER + json.dumps(self._frames_seen))
