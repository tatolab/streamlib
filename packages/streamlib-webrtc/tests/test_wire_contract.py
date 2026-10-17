# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What `WhepPlayer` writes, read back as what a decoder casts it to.

The records are packed and sent over real iceoryx2 links, because the thing
worth pinning is that every key survives the wire — that `bitstream` arrives as
`bytes` and not as a list of integers, and that the engine's own casts accept
the map without a key missing.

GPU-free: the links are wired directly and no runtime is started.
"""

import os
from collections.abc import Iterator
from typing import Any

import pytest

from helper_process_link_wiring import wire_one_link
from played_media_stand_ins import (
    ANNEX_B_ACCESS_UNIT,
    OPUS_PACKET,
    AccessUnitTheSessionPlayed,
    OpusPacketTheSessionPlayed,
)
from tatolab.webrtc.processors import (
    encoded_audio_packet_bag,
    encoded_video_frame_bag,
)
from tatolab.runtime import _engine
from tatolab.stream import (
    EncodedAudioPacket,
    EncodedVideoFrame,
    NodeLinkDataAccess,
    msgpack,
)

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")

INPUT_PORT = "encoded_in"
OUTPUT_PORT = "encoded_out"


class WiredLinkUnderTest:
    """One live link, from the writing end to the reading end."""

    def __init__(
        self, source: NodeLinkDataAccess, destination: NodeLinkDataAccess
    ) -> None:
        self.source = source
        self.destination = destination

    def round_trip(self, record: "dict[str, Any]") -> bytes:
        self.source.write_to_output_port(OUTPUT_PORT, msgpack.pack(record))
        payload = self.destination.read_from_input_port(INPUT_PORT)
        assert payload is not None, "the wired input received nothing"
        return payload

    def round_trip_unpacked(self, record: "dict[str, Any]") -> "dict[str, Any]":
        return msgpack.unpack(self.round_trip(record))


@pytest.fixture
def wired_link(request: pytest.FixtureRequest) -> Iterator[WiredLinkUnderTest]:
    """A source and a destination joined by one link.

    Service names carry the test's own name because every test in this
    process shares one iceoryx2 domain.
    """
    unique = f"webrtcwire{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/encoded"
    notify_service_name = f"{unique}_dest/notify"

    destination = _engine.open_node_link_data_access_for_helper_process()
    source = _engine.open_node_link_data_access_for_helper_process()
    wire_one_link(
        source,
        OUTPUT_PORT,
        destination,
        INPUT_PORT,
        channel_service_name,
        notify_service_name,
    )
    yield WiredLinkUnderTest(source, destination)


def test_a_played_access_unit_casts_to_an_encoded_video_frame(
    wired_link: WiredLinkUnderTest,
):
    payload = wired_link.round_trip(encoded_video_frame_bag(AccessUnitTheSessionPlayed()))

    frame = EncodedVideoFrame.from_bytes(payload)

    assert frame.codec == "h264"
    assert frame.annex_b_access_unit_bytes == ANNEX_B_ACCESS_UNIT
    assert frame.is_sync_point is True
    assert (frame.group_index, frame.sequence_index) == (3, 11)
    assert (frame.width, frame.height) == (320, 180)
    assert frame.color is None


def test_the_access_units_bitstream_crosses_the_wire_as_bytes(
    wired_link: WiredLinkUnderTest,
):
    """A `list` here would read back equal-looking and be unusable as a buffer
    by a muxer, a socket, or a decoder in another language."""
    record = wired_link.round_trip_unpacked(
        encoded_video_frame_bag(AccessUnitTheSessionPlayed())
    )

    assert type(record["bitstream"]) is bytes


def test_a_colour_the_stream_described_survives_as_the_casts_own_type(
    wired_link: WiredLinkUnderTest,
):
    described = AccessUnitTheSessionPlayed(
        color={"primaries": "bt709", "transfer": "srgb", "matrix": "bt709",
               "range": "limited"}
    )  # fmt: skip

    frame = EncodedVideoFrame.from_bytes(
        wired_link.round_trip(encoded_video_frame_bag(described))
    )

    assert frame.color is not None
    assert frame.color.primaries == "bt709"
    assert frame.color.range == "limited"


def test_a_stream_that_described_no_colour_carries_no_colour_key(
    wired_link: WiredLinkUnderTest,
):
    """Absent means unspecified; a map of nulls would mean something else."""
    record = wired_link.round_trip_unpacked(
        encoded_video_frame_bag(AccessUnitTheSessionPlayed())
    )

    assert "color" not in record


def test_a_played_packet_casts_to_an_encoded_audio_packet(
    wired_link: WiredLinkUnderTest,
):
    payload = wired_link.round_trip(encoded_audio_packet_bag(OpusPacketTheSessionPlayed()))

    packet = EncodedAudioPacket.from_bytes(payload)

    assert packet.codec == "opus"
    assert packet.opus_packet_bytes == OPUS_PACKET
    assert packet.is_sync_point is True
    assert (packet.group_index, packet.sequence_index) == (7, 7)
    assert (packet.sample_rate, packet.channels, packet.sample_count) == (48_000, 1, 960)
    assert packet.pre_skip == 0


def test_a_mono_stream_stays_mono_across_the_wire(wired_link: WiredLinkUnderTest):
    """The channel count comes from each packet's TOC byte; the answer's rtpmap
    says 2 for every Opus stream ever negotiated."""
    mono = wired_link.round_trip(
        encoded_audio_packet_bag(OpusPacketTheSessionPlayed(channels=1))
    )
    stereo = wired_link.round_trip(
        encoded_audio_packet_bag(OpusPacketTheSessionPlayed(channels=2))
    )

    assert EncodedAudioPacket.from_bytes(mono).channels == 1
    assert EncodedAudioPacket.from_bytes(stereo).channels == 2
