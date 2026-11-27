# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The many-input node `test_inbound_link_naming.py` drives, its feeders and its reporter.

In its own module because a helper process imports each class by its import
path, and a class declared inside a pytest module would have the child import
the test suite.
"""

from typing import Callable
import json

from json_messages import JsonMessagesInOrder, json_bytes, json_read
from tatolab.stream import (
    EncodedVideo,
    Input,
    InputPin,
    OutputPin,
    RuntimeContextLimitedAccess,
    log,
    node,
)


@node(execution="reactive")
def ReportsWhichLinkEachBagCameFrom(
    *,
    tracks: InputPin[JsonMessagesInOrder],
    attributions_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """One input port, any number of producers into it.

    The shape every many-track sink has: it never declares a port per producer,
    it reads the one port and asks each message which link it arrived on.
    """

    # Links are wired before setup() runs, so a sink knows here — before a
    # single message has arrived — how many producers it owes.
    links_at_setup = sorted(tracks.inbound_link_names())

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        message = tracks.read_message()
        if message is None:
            return
        attributions_to_downstream.write(
            json_bytes(
                {
                    "value": json_read(message.payload)["value"],
                    "arrived_on": message.inbound_link,
                    "links_at_setup": links_at_setup,
                }
            ),
        )

    return each_frame


@node(execution="continuous", interval_ms=100)
def FeedsOneValueSource(
    *,
    value: Input[str] = Input("unset"),
    bags_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Writes a message carrying its `value` field, again every tick.

    Again rather than once: a message written before the far end of its link
    is listening would be lost, and the attribution asserted on is the same for
    every message of one producer.
    """

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        bags_to_downstream.write(json_bytes({"value": value.get()}))

    return each_frame


@node(execution="reactive")
def ReportsEachAttributionSink(
    *,
    attributions_from_upstream: InputPin[JsonMessagesInOrder],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports every attribution it reads as `MARKER:ATTRIBUTION <json>`."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        unit = attributions_from_upstream.read()
        if unit is not None:
            log.info("MARKER:ATTRIBUTION " + json.dumps(json_read(unit)))

    return each_frame
