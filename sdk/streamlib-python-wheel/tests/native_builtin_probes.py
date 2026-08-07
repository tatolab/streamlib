# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probe that reads a native built-in's frames from a helper process.

Reports the first two video-frame bags it receives as one JSON marker line,
riding the child→parent log forwarding — the boundary under test is native
production in the app process reaching a Python processor in its own child.
"""

import json

from streamlib import input, log, processor

RESULT_MARKER = "MARKER:FRAMES_SEEN "


@processor
class VideoFrameProbe:
    """Reports the first two video-frame bags the native source publishes."""

    def __init__(self) -> None:
        self.bags_seen = []

    @input(delivery_profile="every_sample")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        if len(self.bags_seen) >= 2:
            return
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None:
            return
        self.bags_seen.append(dict(bag))
        if len(self.bags_seen) == 2:
            log.info(RESULT_MARKER + json.dumps(self.bags_seen))
