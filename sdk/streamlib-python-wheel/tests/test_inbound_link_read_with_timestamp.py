# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Reading a message's payload with its link and its producer's stamp.

A many-track sink needs both at once: the link says which producer, and the
stamp is the one the producer wrote rather than the moment of the read. The
payload comes back as the bytes the producer wrote, or cast by `into=`.

GPU-free by construction. The links are real iceoryx2 ports and the context is
a helper-process one opened directly on them, so nothing here starts a runtime
or touches a device.
"""

import os
from collections.abc import Iterator
from dataclasses import dataclass
from typing import Any

import pytest

from tatolab.runtime import _engine
from tatolab.stream import (
    MsgpackCast,
    NodeLinkDataAccess,
    RuntimeContextFullAccess,
    msgpack,
)

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)

INPUT_PORT = "tracks"
OUTPUT_PORT = "messages_to_downstream"

VIDEO_STAMP_NS = 1_700_000_000
AUDIO_STAMP_NS = 1_700_020_000


@dataclass
class TrackDescription(MsgpackCast):
    """A record a fan-in read can cast into."""

    codec: str


class TwoLinksIntoOnePort:
    """Two producers wired into one input port, and the context reading them.

    A link is named by the source channel it subscribed to, not by the link id
    the wiring carried, so the channel name is what a read hands back and what
    these tests assert on.
    """

    def __init__(
        self,
        context: RuntimeContextFullAccess,
        sources: "dict[str, NodeLinkDataAccess]",
        channel_named: "dict[str, str]",
    ) -> None:
        self.context = context
        self.sources = sources
        self.channel_named = channel_named

    def deliver(self, kind: str, payload: bytes, timestamp_ns: int) -> None:
        self.sources[kind].write_to_output_port(OUTPUT_PORT, payload, timestamp_ns)


@pytest.fixture
def two_links_into_one_port(
    request: pytest.FixtureRequest,
) -> Iterator[TwoLinksIntoOnePort]:
    """Service names carry the test's own name because every test in this
    process shares one iceoryx2 domain."""
    unique = f"fanints{os.getpid()}_{request.node.name}"
    notify_service_name = f"{unique}_dest/notify"

    # The destination subscribes first: iceoryx2 drops a send with no
    # subscriber attached.
    destination = _engine.open_node_link_data_access_for_helper_process()
    sources: "dict[str, NodeLinkDataAccess]" = {}
    channel_named: "dict[str, str]" = {}
    for kind in ("video", "audio"):
        channel_service_name = f"{unique}/{kind}"
        destination.wire_input_link(
            INPUT_PORT, channel_service_name, channel_service_name,
            notify_service_name,
            "read_next_in_order", 8, 8, 2, 2, f"L-{unique}-{kind}",
        )  # fmt: skip
        source = _engine.open_node_link_data_access_for_helper_process()
        source.wire_output_link(
            OUTPUT_PORT, channel_service_name, notify_service_name,
            1024, 1 << 20, 8, 2, 2, f"L-{unique}-{kind}",
        )  # fmt: skip
        sources[kind] = source
        channel_named[kind] = channel_service_name

    context = _engine.open_runtime_context_full_access_for_helper_process(
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


def test_a_message_arrives_as_its_payload_bytes_with_its_link_and_its_producers_stamp(
    two_links_into_one_port: TwoLinksIntoOnePort,
):
    two_links_into_one_port.deliver("video", b"h264", VIDEO_STAMP_NS)

    read = two_links_into_one_port.context.inputs.read_from_inbound_link_with_timestamp(
        INPUT_PORT
    )

    assert read is not None
    payload, inbound_link, timestamp_ns = read
    assert payload == b"h264"
    assert type(payload) is bytes
    assert inbound_link == two_links_into_one_port.channel_named["video"]
    assert timestamp_ns == VIDEO_STAMP_NS


def test_into_casts_the_payload_on_both_fan_in_reads(
    two_links_into_one_port: TwoLinksIntoOnePort,
):
    two_links_into_one_port.deliver("video", msgpack.pack({"codec": "h264"}), VIDEO_STAMP_NS)
    two_links_into_one_port.deliver("video", msgpack.pack({"codec": "h265"}), VIDEO_STAMP_NS)
    inputs = two_links_into_one_port.context.inputs
    video_link = two_links_into_one_port.channel_named["video"]

    assert inputs.read_from_inbound_link_with_timestamp(
        INPUT_PORT, into=TrackDescription
    ) == (TrackDescription(codec="h264"), video_link, VIDEO_STAMP_NS)
    assert inputs.read_from_inbound_link(INPUT_PORT, into=TrackDescription) == (
        TrackDescription(codec="h265"),
        video_link,
    )


def test_each_producers_own_stamp_survives_the_read_that_names_it(
    two_links_into_one_port: TwoLinksIntoOnePort,
):
    """The stamp is per message, not per port: two producers writing different
    instants must not collapse onto whichever was read last."""
    two_links_into_one_port.deliver("video", b"h264", VIDEO_STAMP_NS)
    two_links_into_one_port.deliver("audio", b"opus", AUDIO_STAMP_NS)

    stamp_by_link = {
        inbound_link: timestamp_ns
        for _payload, inbound_link, timestamp_ns in read_everything_waiting(
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


def test_the_untimestamped_fan_in_read_yields_the_payload_bytes_and_the_link(
    two_links_into_one_port: TwoLinksIntoOnePort,
):
    two_links_into_one_port.deliver("audio", b"opus", AUDIO_STAMP_NS)

    read = two_links_into_one_port.context.inputs.read_from_inbound_link(INPUT_PORT)

    assert read == (
        b"opus",
        two_links_into_one_port.channel_named["audio"],
    )


def test_a_link_is_named_by_what_the_engine_wired_it_under_not_by_its_channel(
    request: pytest.FixtureRequest,
):
    """The link name and the channel arrive as two separate arguments, and a
    read hands back the link name. Wired with a link name that is not its
    channel, the read names the link and never the channel.
    """
    unique = f"linkname{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/channel-the-link-is-not-named-by"
    inbound_link_name = "bench-cam-a1b2/CameraSource/video"
    notify_service_name = f"{unique}_dest/notify"

    destination = _engine.open_node_link_data_access_for_helper_process()
    destination.wire_input_link(
        INPUT_PORT, channel_service_name, inbound_link_name,
        notify_service_name,
        "read_next_in_order", 8, 8, 2, 1, f"L-{unique}",
    )  # fmt: skip
    source = _engine.open_node_link_data_access_for_helper_process()
    source.wire_output_link(
        OUTPUT_PORT, channel_service_name, notify_service_name,
        1024, 1 << 20, 8, 2, 1, f"L-{unique}",
    )  # fmt: skip
    context = _engine.open_runtime_context_full_access_for_helper_process(
        {}, destination, "runtime-under-test", "processor-under-test"
    )

    assert context.inputs.inbound_link_names(INPUT_PORT) == [inbound_link_name]

    source.write_to_output_port(OUTPUT_PORT, b"h264", VIDEO_STAMP_NS)

    assert context.inputs.read_from_inbound_link(INPUT_PORT) == (
        b"h264",
        inbound_link_name,
    )
