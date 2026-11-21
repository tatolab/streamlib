# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The many-input node `test_inbound_link_naming.py` drives, its feeders and its reporter.

In its own module because a helper process imports each class by its import
path, and a class declared inside a pytest module would have the child import
the test suite.
"""

import json

from json_messages import JsonMessagesInOrder, json_bytes, json_read
from tatolab.stream import (
    EncodedVideo,
    InputPin,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    node,
)


@node
class ReportsWhichLinkEachBagCameFrom:
    """One input port, any number of producers into it.

    The shape every many-track sink has: it never declares a port per producer,
    it reads the one port and asks each message which link it arrived on.
    """

    tracks: InputPin[JsonMessagesInOrder]

    attributions_to_downstream: OutputPin[EncodedVideo]

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Links are wired before setup() runs, so a sink knows here — before a
        # single message has arrived — how many producers it owes.
        self._links_at_setup = sorted(self.tracks.inbound_link_names())

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        message = self.tracks.read_message()
        if message is None:
            return
        self.attributions_to_downstream.write(
            json_bytes(
                {
                    "value": json_read(message.payload)["value"],
                    "arrived_on": message.inbound_link,
                    "links_at_setup": self._links_at_setup,
                }
            ),
        )


@node(execution="continuous", interval_ms=100)
class FeedsOneValueSource:
    """Writes a message carrying its `value` field, again every tick.

    Again rather than once: a message written before the far end of its link
    is listening would be lost, and the attribution asserted on is the same for
    every message of one producer.
    """

    value: str = "unset"

    bags_to_downstream: OutputPin[EncodedVideo]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        self.bags_to_downstream.write(json_bytes({"value": self.value}))


@node
class ReportsEachAttributionSink:
    """Reports every attribution it reads as `MARKER:ATTRIBUTION <json>`."""

    attributions_from_upstream: InputPin[JsonMessagesInOrder]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        unit = self.attributions_from_upstream.read()
        if unit is not None:
            log.info("MARKER:ATTRIBUTION " + json.dumps(json_read(unit)))
