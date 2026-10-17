# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Reading a message with what the engine knows about it: `read_message`.

A decoder needs, on every message, whether it starts a group and whether its
link lost anything since the previous one, so it can discard to the next group.
`read_message` hands both back beside the payload, the stamp and the link.

GPU-free by construction. The link is a real iceoryx2 channel and the context is
a helper-process one opened directly on it, so nothing here starts a runtime or
touches a device.
"""

import os
from collections.abc import Iterator
from dataclasses import FrozenInstanceError, dataclass

import pytest

from tatolab.runtime import _engine
from tatolab.stream import (
    Message,
    MsgpackCast,
    NodeLinkDataAccess,
    RuntimeContextFullAccess,
    msgpack,
)

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)

INPUT_PORT = "encoded_video"
OUTPUT_PORT = "encoded_video_to_downstream"
INPUT_PORT_RING_DEPTH = 2
STAMP_NS = 1_700_000_000


@dataclass
class AccessUnitDescription(MsgpackCast):
    """A record `into=` can cast a payload into."""

    codec: str


@dataclass
class OneLinkIntoOnePort:
    """A producer wired into an `ordered` input port two messages deep, and the
    context reading it."""

    context: RuntimeContextFullAccess
    source: NodeLinkDataAccess
    inbound_link: str

    def deliver(self, payload: bytes, *, starts_group: bool = True) -> None:
        self.source.write_to_output_port(OUTPUT_PORT, payload, STAMP_NS, starts_group)


@pytest.fixture
def one_link_into_one_port(request: pytest.FixtureRequest) -> Iterator[OneLinkIntoOnePort]:
    """Service names carry the test's own name because every test in this
    process shares one iceoryx2 domain."""
    unique = f"readmessage{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/encoded_video"
    notify_service_name = f"{unique}_dest/notify"

    # The destination subscribes first: iceoryx2 drops a send with no
    # subscriber attached.
    destination = _engine.open_node_link_data_access_for_helper_process()
    destination.wire_input_link(
        INPUT_PORT, channel_service_name, channel_service_name,
        notify_service_name,
        "read_next_in_order", 8, INPUT_PORT_RING_DEPTH, 2, 1, f"L-{unique}",
    )  # fmt: skip
    source = _engine.open_node_link_data_access_for_helper_process()
    source.wire_output_link(
        OUTPUT_PORT, channel_service_name, notify_service_name,
        1024, 1 << 20, 8, 2, 1, f"L-{unique}",
    )  # fmt: skip
    context = _engine.open_runtime_context_full_access_for_helper_process(
        {}, destination, "runtime-under-test", "processor-under-test"
    )
    yield OneLinkIntoOnePort(context, source, channel_service_name)


def test_a_message_arrives_with_its_stamp_its_group_flag_and_its_link(
    one_link_into_one_port: OneLinkIntoOnePort,
):
    one_link_into_one_port.deliver(b"keyframe")
    one_link_into_one_port.deliver(b"delta", starts_group=False)
    inputs = one_link_into_one_port.context.inputs
    inbound_link = one_link_into_one_port.inbound_link

    assert inputs.read_message(INPUT_PORT) == Message(
        payload=b"keyframe",
        timestamp_ns=STAMP_NS,
        starts_group=True,
        lost_since_last_read=0,
        inbound_link=inbound_link,
    )
    assert inputs.read_message(INPUT_PORT) == Message(
        payload=b"delta",
        timestamp_ns=STAMP_NS,
        starts_group=False,
        lost_since_last_read=0,
        inbound_link=inbound_link,
    )
    assert inputs.read_message(INPUT_PORT) is None


def test_messages_the_link_lost_are_told_to_the_next_read_and_to_no_read_after_it(
    one_link_into_one_port: OneLinkIntoOnePort,
):
    """Fail-without-fix: leave `lost_since_last_read` at zero and the reader
    reads two messages that look contiguous over a gap of two."""
    messages_past_the_ring = 2
    inputs = one_link_into_one_port.context.inputs
    one_link_into_one_port.deliver(b"first")
    first = inputs.read_message(INPUT_PORT)

    for index in range(INPUT_PORT_RING_DEPTH + messages_past_the_ring):
        one_link_into_one_port.deliver(f"after-{index}".encode(), starts_group=False)
    after_the_overrun: "list[Message[bytes]]" = []
    while (message := inputs.read_message(INPUT_PORT)) is not None:
        after_the_overrun.append(message)

    assert first is not None and first.lost_since_last_read == 0
    assert [(message.payload, message.lost_since_last_read) for message in after_the_overrun] == [
        (b"after-2", messages_past_the_ring),
        (b"after-3", 0),
    ]


def test_into_casts_the_payload_the_way_read_does(
    one_link_into_one_port: OneLinkIntoOnePort,
):
    one_link_into_one_port.deliver(msgpack.pack({"codec": "avc1.64001f"}), starts_group=False)

    message = one_link_into_one_port.context.inputs.read_message(
        INPUT_PORT, into=AccessUnitDescription
    )

    assert message is not None
    assert message.payload == AccessUnitDescription(codec="avc1.64001f")
    assert message.starts_group is False
    assert message.inbound_link == one_link_into_one_port.inbound_link


def test_a_message_is_frozen(one_link_into_one_port: OneLinkIntoOnePort):
    one_link_into_one_port.deliver(b"keyframe")
    message = one_link_into_one_port.context.inputs.read_message(INPUT_PORT)

    assert message is not None
    with pytest.raises(FrozenInstanceError):
        message.lost_since_last_read = 0  # type: ignore[misc]
