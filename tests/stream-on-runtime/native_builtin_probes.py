# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probe that reads a native built-in's frames from a processor interpreter.

Reports the first two video-frame bags it receives as one JSON marker line on
`tatolabd`'s standard error — the boundary under test is native production in
`tatolabd` reaching a Python processor in its own interpreter.
"""

import json

from tatolab.stream import log, node
from tatolab.stream.msgpack import unpack

RESULT_MARKER = "MARKER:FRAMES_SEEN "


@node
class VideoFrameProbe:
    """Reports the first two video-frame messages the native source publishes."""

    def __init__(self) -> None:
        self.messages_seen = []

    @node.input(delivery_profile="ordered")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        if len(self.messages_seen) >= 2:
            return
        payload = ctx.inputs.read("video_from_upstream")
        if payload is None:
            return
        self.messages_seen.append(unpack(payload))
        if len(self.messages_seen) == 2:
            log.info(RESULT_MARKER + json.dumps(self.messages_seen))
