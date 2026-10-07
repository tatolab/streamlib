# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.EncodedVideoFrame` — the encoded bag's cast, over a live link.

An encoded frame carries its payload inline, so what has to survive the wire
is the payload's msgpack *type*: `bin`, which reaches Python as `bytes`. That
is what these tests drive, over real wired iceoryx2 ports rather than a
stand-in, because a bag has to survive the wire before `into=` has anything to
cast.

Both ends live on this thread because iceoryx2's ports are `!Send`, and the
destination is wired first because a send with no subscriber attached is
dropped. No engine and no GPU: the ports are wired directly, which is what
keeps this half of the proof in CI.
"""

import os
from collections.abc import Iterator
from typing import Any

import pytest

from tatolab.runtime import _engine
from tatolab.stream import ColorInfo, EncodedVideoFrame, NodeLinkDataAccess
from tatolab.stream.encoded_video_frame import _CODECS_ON_THE_WIRE

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")

OUTPUT_PORT = "encoded_video_to_downstream"
INPUT_PORT = "encoded_video_from_upstream"

# A one-NAL access unit: the four-byte Annex-B start code, an IDR NAL header,
# and a byte of payload. Short on purpose — nothing here decodes it, and what
# is under test is that the bytes arrive as bytes.
ANNEX_B_ACCESS_UNIT = b"\x00\x00\x00\x01\x65\x88"

BT709_COLOR_ON_THE_WIRE = {
    "primaries": "bt709",
    "transfer": "bt709",
    "matrix": "bt709",
    "range": "limited",
}


def encoded_frame_bag(**overrides: Any) -> "dict[str, Any]":
    """A sync-point H.264 access unit at the coded extent of a 320×180 source."""
    bag: "dict[str, Any]" = {
        "codec": "h264",
        "bitstream": ANNEX_B_ACCESS_UNIT,
        "is_sync_point": True,
        "group_index": 0,
        "sequence_index": 0,
        "width": 320,
        "height": 192,
        "color": dict(BT709_COLOR_ON_THE_WIRE),
    }
    bag.update(overrides)
    return bag


class WiredLinkUnderTest:
    """One live link, from the writing end to the reading end."""

    def __init__(
        self, source: NodeLinkDataAccess, destination: NodeLinkDataAccess
    ) -> None:
        self.source = source
        self.destination = destination

    def deliver(self, bag: "dict[str, Any]") -> None:
        self.source.write_to_output_port(OUTPUT_PORT, bag)


@pytest.fixture
def wired_link(request: pytest.FixtureRequest) -> Iterator[WiredLinkUnderTest]:
    """A source and a destination joined by one link.

    Service names carry the test's own name because every test in this
    process shares one iceoryx2 domain — a fixed name would let one test's
    channel meet the next one's.
    """
    unique = f"encvid{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/encoded_video"
    notify_service_name = f"{unique}_dest/notify"
    link_id = f"L-{unique}"

    destination = _engine.open_node_link_data_access_for_helper_process()
    destination.wire_input_link(
        INPUT_PORT,
        channel_service_name,
        channel_service_name,
        notify_service_name,
        "read_next_in_order",
        8,
        8,
        2,
        1,
        link_id,
    )
    source = _engine.open_node_link_data_access_for_helper_process()
    source.wire_output_link(
        OUTPUT_PORT,
        channel_service_name,
        notify_service_name,
        1024,
        1 << 20,
        8,
        2,
        1,
        link_id,
    )
    yield WiredLinkUnderTest(source, destination)


def test_the_bitstream_crosses_the_wire_as_bytes(wired_link: WiredLinkUnderTest):
    """The contract underneath the cast: `bitstream` is a byte buffer on the
    wire, so an untyped read hands back `bytes` rather than a list of numbers.

    A producer whose access unit encoded as a msgpack array would still read
    back equal-looking data here — as a `list`, unreadable as a buffer by a
    muxer, a socket, or a consumer in another language.
    """
    wired_link.deliver(encoded_frame_bag())

    bag = wired_link.destination.read_from_input_port(INPUT_PORT)

    assert bag is not None
    assert type(bag["bitstream"]) is bytes
    assert bag["bitstream"] == ANNEX_B_ACCESS_UNIT


def test_every_wire_key_survives_the_read_into_an_encoded_video_frame(
    wired_link: WiredLinkUnderTest,
):
    """All eight keys, off a live link, in one assertion each — this is the
    lock on the wire contract the codec built-ins publish against."""
    wired_link.deliver(
        encoded_frame_bag(is_sync_point=False, group_index=3, sequence_index=91)
    )

    frame = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=EncodedVideoFrame
    )

    assert isinstance(frame, EncodedVideoFrame)
    assert frame.codec == "h264"
    assert frame.annex_b_access_unit_bytes == ANNEX_B_ACCESS_UNIT
    assert frame.is_sync_point is False
    assert frame.group_index == 3
    assert frame.sequence_index == 91
    assert frame.width == 320
    assert frame.height == 192
    assert frame.color == ColorInfo(
        primaries="bt709", transfer="bt709", matrix="bt709", range="limited"
    )


@pytest.mark.parametrize("codec", _CODECS_ON_THE_WIRE)
def test_both_elementary_streams_read_through_the_same_cast(
    wired_link: WiredLinkUnderTest, codec: str
):
    """One cast serves both codecs: `codec` is metadata on an otherwise
    identical bag, which is why the pair differs in an enumerant and a name."""
    wired_link.deliver(encoded_frame_bag(codec=codec))

    frame = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=EncodedVideoFrame
    )

    assert isinstance(frame, EncodedVideoFrame)
    assert frame.codec == codec


def test_a_key_this_cast_does_not_read_does_not_break_the_read(
    wired_link: WiredLinkUnderTest,
):
    """The bag map is open: the day a producer adds a key must not be the day
    every `read(port, into=EncodedVideoFrame)` starts raising."""
    wired_link.deliver(encoded_frame_bag(a_future_key="ignored"))

    frame = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=EncodedVideoFrame
    )

    assert isinstance(frame, EncodedVideoFrame)
    assert frame.sequence_index == 0
    assert not hasattr(frame, "a_future_key")


def test_a_bag_with_no_color_reads_as_unspecified(wired_link: WiredLinkUnderTest):
    """`color` is absent-means-unspecified — the H.273 rule — so a producer
    that writes none is describing an unspecified stream, not an unreadable
    bag."""
    bag = encoded_frame_bag()
    del bag["color"]
    wired_link.deliver(bag)

    frame = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=EncodedVideoFrame
    )

    assert isinstance(frame, EncodedVideoFrame)
    assert frame.color is None
