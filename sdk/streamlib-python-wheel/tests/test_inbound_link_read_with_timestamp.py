# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Reading a bag's link and its producer's stamp in one call.

Fan-in already named the link a bag arrived on, and the timestamped read
already handed back a stamp, but no read did both — so a many-track sink could
name its producers or restate their timing, never both at once. A publisher
that puts several tracks on one wire needs both: the link says which track, and
the stamp is the one the producer wrote rather than the moment of the read.

GPU-free by construction. The links are real iceoryx2 ports and the context is
a helper-process one opened directly on them, so nothing here starts a runtime
or touches a device.
"""

import os
from collections.abc import Iterator
from typing import Any

import pytest

from streamlib import RuntimeContextFullAccess
from streamlib._engine import ProcessorLinkDataAccess

INPUT_PORT = "tracks"
OUTPUT_PORT = "bags_to_downstream"

VIDEO_STAMP_NS = 1_700_000_000
AUDIO_STAMP_NS = 1_700_020_000


class TwoLinksIntoOnePort:
    """Two producers wired into one input port, and the context reading them.

    A link is named by the source channel it subscribed to, not by the link id
    the wiring carried, so the channel name is what a read hands back and what
    these tests assert on.
    """

    def __init__(
        self,
        context: RuntimeContextFullAccess,
        sources: "dict[str, ProcessorLinkDataAccess]",
        channel_named: "dict[str, str]",
    ) -> None:
        self.context = context
        self.sources = sources
        self.channel_named = channel_named

    def deliver(self, kind: str, bag: "dict[str, Any]", timestamp_ns: int) -> None:
        self.sources[kind].write_to_output_port(OUTPUT_PORT, bag, timestamp_ns)


@pytest.fixture
def two_links_into_one_port(
    request: pytest.FixtureRequest,
) -> Iterator[TwoLinksIntoOnePort]:
    """Service names carry the pid and the test's own name because iceoryx2
    service state is machine-global and outlives a crashed process."""
    unique = f"fanints{os.getpid()}_{request.node.name}"
    notify_service_name = f"{unique}_dest/notify"

    # The destination subscribes first: iceoryx2 drops a send with no
    # subscriber attached.
    destination = ProcessorLinkDataAccess()
    sources: "dict[str, ProcessorLinkDataAccess]" = {}
    channel_named: "dict[str, str]" = {}
    for kind in ("video", "audio"):
        channel_service_name = f"{unique}/{kind}"
        destination.wire_input_link(
            INPUT_PORT, channel_service_name, notify_service_name,
            "read_next_in_order", 8, 2, 2, f"L-{unique}-{kind}",
        )  # fmt: skip
        source = ProcessorLinkDataAccess()
        source.wire_output_link(
            OUTPUT_PORT, channel_service_name, notify_service_name,
            1024, 1 << 20, 8, 2, 2, f"L-{unique}-{kind}",
        )  # fmt: skip
        sources[kind] = source
        channel_named[kind] = channel_service_name

    context = RuntimeContextFullAccess.open_for_helper_process(
        {}, destination, "runtime-under-test", "processor-under-test"
    )
    yield TwoLinksIntoOnePort(context, sources, channel_named)


def read_everything_waiting(
    context: RuntimeContextFullAccess,
) -> "list[tuple[Any, str, int]]":
    """Drain the port, which two links may interleave in either order."""
    drained: "list[tuple[Any, str, int]]" = []
    while (read := context.inputs.read_from_inbound_link_with_timestamp(INPUT_PORT)) is not None:
        drained.append(read)
    return drained


def test_a_bag_arrives_with_both_its_link_and_its_producers_stamp(
    two_links_into_one_port: TwoLinksIntoOnePort,
):
    two_links_into_one_port.deliver("video", {"codec": "h264"}, VIDEO_STAMP_NS)

    read = two_links_into_one_port.context.inputs.read_from_inbound_link_with_timestamp(
        INPUT_PORT
    )

    assert read is not None
    bag, inbound_link, timestamp_ns = read
    assert bag == {"codec": "h264"}
    assert inbound_link == two_links_into_one_port.channel_named["video"]
    assert timestamp_ns == VIDEO_STAMP_NS


def test_each_producers_own_stamp_survives_the_read_that_names_it(
    two_links_into_one_port: TwoLinksIntoOnePort,
):
    """The stamp is per bag, not per port: two producers writing different
    instants must not collapse onto whichever was read last."""
    two_links_into_one_port.deliver("video", {"codec": "h264"}, VIDEO_STAMP_NS)
    two_links_into_one_port.deliver("audio", {"codec": "opus"}, AUDIO_STAMP_NS)

    stamp_by_link = {
        inbound_link: timestamp_ns
        for _bag, inbound_link, timestamp_ns in read_everything_waiting(
            two_links_into_one_port.context
        )
    }

    assert stamp_by_link == {
        two_links_into_one_port.channel_named["video"]: VIDEO_STAMP_NS,
        two_links_into_one_port.channel_named["audio"]: AUDIO_STAMP_NS,
    }


def test_an_empty_port_reads_as_nothing_rather_than_a_triple_of_nones(
    two_links_into_one_port: TwoLinksIntoOnePort,
):
    """The shape `read_from_inbound_link` already has, kept — a caller
    unpacks a triple only once it has one."""
    assert (
        two_links_into_one_port.context.inputs.read_from_inbound_link_with_timestamp(
            INPUT_PORT
        )
        is None
    )


def test_the_untimestamped_fan_in_read_is_unchanged(
    two_links_into_one_port: TwoLinksIntoOnePort,
):
    """Both reads share one body now; this is what pins the older one's shape."""
    two_links_into_one_port.deliver("audio", {"codec": "opus"}, AUDIO_STAMP_NS)

    read = two_links_into_one_port.context.inputs.read_from_inbound_link(INPUT_PORT)

    assert read == (
        {"codec": "opus"},
        two_links_into_one_port.channel_named["audio"],
    )
