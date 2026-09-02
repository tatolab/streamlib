# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probe that reads a codec round trip's decoded frames from a helper process.

Reports the first two decoded video-frame bags it receives as one JSON marker
line, riding the child→parent log forwarding — what is under test is the
decoder's published output reaching an ordinary Python consumer, so the probe
reads bags and casts nothing.
"""

import json

from streamlib import input, log, processor

DECODED_FRAMES_MARKER = "MARKER:DECODED_FRAMES_SEEN "


@processor
class DecodedVideoFrameProbe:
    """Reports the first two video-frame bags the decoder publishes."""

    def __init__(self) -> None:
        self.bags_seen = []

    @input(delivery_profile="ordered")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        if len(self.bags_seen) >= 2:
            return
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None:
            return
        self.bags_seen.append(dict(bag))
        if len(self.bags_seen) == 2:
            log.info(DECODED_FRAMES_MARKER + json.dumps(self.bags_seen))
