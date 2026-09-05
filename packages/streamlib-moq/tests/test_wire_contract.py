# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What `MoqBroadcastSubscriber` writes, read back as what a decoder casts it to.

The bags go over real iceoryx2 links rather than being compared as dicts,
because the thing worth pinning is that every key survives the wire — that
`bitstream` arrives as `bytes` and not as a list of integers, and that the
engine's own casts accept the map without a key missing.

GPU-free: the links are wired directly and no runtime is started.
"""

import os
from collections.abc import Iterator
from dataclasses import dataclass
from typing import Any

import pytest

from streamlib import EncodedAudioPacket, EncodedVideoFrame
from streamlib._engine import ProcessorLinkDataAccess
from streamlib_moq.processors import (
    encoded_audio_packet_bag,
    encoded_video_frame_bag,
)

INPUT_PORT = "encoded_in"
OUTPUT_PORT = "encoded_out"

ANNEX_B_ACCESS_UNIT = b"\x00\x00\x00\x01\x67\x42\xe0\x1f\x00\x00\x00\x01\x65\x88\x84"
OPUS_PACKET = b"\x78\x01\x02\x03"


@dataclass(frozen=True)
class AnAccessUnitOffTheBroadcast:
    """What the native layer hands the bag spelling, stated by a test."""

    codec: str = "h264"
    bitstream: bytes = ANNEX_B_ACCESS_UNIT
    is_sync_point: bool = True
    group_index: int = 3
    sequence_index: int = 11
    width: int = 320
    height: int = 180
    color: "dict[str, str] | None" = None


@dataclass(frozen=True)
class AnOpusPacketOffTheBroadcast:
    bitstream: bytes = OPUS_PACKET
    is_sync_point: bool = True
    group_index: int = 7
    sequence_index: int = 7
    sample_rate: int = 48_000
    channels: int = 1
    sample_count: int = 960
    pre_skip: int = 312


class WiredLinkUnderTest:
    """One live link, from the writing end to the reading end."""

    def __init__(
        self, source: ProcessorLinkDataAccess, destination: ProcessorLinkDataAccess
    ) -> None:
        self.source = source
        self.destination = destination

    def round_trip(self, bag: "dict[str, Any]") -> "dict[str, Any]":
        self.source.write_to_output_port(OUTPUT_PORT, bag)
        read = self.destination.read_from_input_port(INPUT_PORT)
        assert read is not None, "the wired input received nothing"
        return read


@pytest.fixture
def wired_link(request: pytest.FixtureRequest) -> Iterator[WiredLinkUnderTest]:
    """A source and a destination joined by one link.

    Service names carry the pid and the test's own name because iceoryx2
    service state is machine-global and outlives a crashed process. The prefix
    is this wheel's own so a webrtc lane running beside this one cannot collide.
    """
    unique = f"moqwire{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/encoded"
    notify_service_name = f"{unique}_dest/notify"
    link_id = f"L-{unique}"

    destination = ProcessorLinkDataAccess()
    destination.wire_input_link(
        INPUT_PORT, channel_service_name, notify_service_name,
        "read_next_in_order", 8, 2, 1, link_id,
    )  # fmt: skip
    source = ProcessorLinkDataAccess()
    source.wire_output_link(
        OUTPUT_PORT, channel_service_name, notify_service_name,
        1024, 1 << 20, 8, 2, 1, link_id,
    )  # fmt: skip
    yield WiredLinkUnderTest(source, destination)


def test_a_received_access_unit_casts_to_an_encoded_video_frame(
    wired_link: WiredLinkUnderTest,
):
    bag = wired_link.round_trip(encoded_video_frame_bag(AnAccessUnitOffTheBroadcast()))

    frame = EncodedVideoFrame(**bag)

    assert frame.codec == "h264"
    assert frame.annex_b_access_unit_bytes == ANNEX_B_ACCESS_UNIT
    assert frame.is_sync_point is True
    assert (frame.group_index, frame.sequence_index) == (3, 11)
    assert (frame.width, frame.height) == (320, 180)
    assert frame.color is None


def test_an_h265_broadcast_carries_its_codec_through_unchanged(
    wired_link: WiredLinkUnderTest,
):
    """The codec is the producer's word, not this wheel's: an extension that
    kept its own list would refuse a codec the engine had just added."""
    bag = wired_link.round_trip(
        encoded_video_frame_bag(AnAccessUnitOffTheBroadcast(codec="h265"))
    )

    assert EncodedVideoFrame(**bag).codec == "h265"


def test_the_access_units_bitstream_crosses_the_wire_as_bytes(
    wired_link: WiredLinkUnderTest,
):
    """A `list` here would read back equal-looking and be unusable as a buffer
    by a muxer, a socket, or a decoder in another language."""
    bag = wired_link.round_trip(encoded_video_frame_bag(AnAccessUnitOffTheBroadcast()))

    assert type(bag["bitstream"]) is bytes


def test_a_colour_the_broadcast_described_survives_as_the_casts_own_type(
    wired_link: WiredLinkUnderTest,
):
    described = AnAccessUnitOffTheBroadcast(
        color={"primaries": "bt709", "transfer": "srgb", "matrix": "bt709",
               "range": "limited"}
    )  # fmt: skip

    frame = EncodedVideoFrame(**wired_link.round_trip(encoded_video_frame_bag(described)))

    assert frame.color is not None
    assert frame.color.primaries == "bt709"
    assert frame.color.range == "limited"


def test_a_broadcast_that_described_no_colour_carries_no_colour_key(
    wired_link: WiredLinkUnderTest,
):
    """Absent means unspecified; a map of nulls would mean something else."""
    bag = wired_link.round_trip(encoded_video_frame_bag(AnAccessUnitOffTheBroadcast()))

    assert "color" not in bag


def test_a_received_packet_casts_to_an_encoded_audio_packet(
    wired_link: WiredLinkUnderTest,
):
    bag = wired_link.round_trip(encoded_audio_packet_bag(AnOpusPacketOffTheBroadcast()))

    packet = EncodedAudioPacket(**bag)

    assert packet.codec == "opus"
    assert packet.opus_packet_bytes == OPUS_PACKET
    assert packet.is_sync_point is True
    assert (packet.group_index, packet.sequence_index) == (7, 7)
    assert (packet.sample_rate, packet.channels, packet.sample_count) == (48_000, 1, 960)


def test_the_producers_pre_skip_is_carried_and_never_recomputed(
    wired_link: WiredLinkUnderTest,
):
    """312 is what this tree's `OpusEncoder` states, and a subscriber that
    substituted the 3840 an OpusHead usually carries would have a decoder trim
    the wrong number of samples off every stream."""
    bag = wired_link.round_trip(encoded_audio_packet_bag(AnOpusPacketOffTheBroadcast()))

    assert EncodedAudioPacket(**bag).pre_skip == 312


def test_a_mono_broadcast_stays_mono_across_the_wire(wired_link: WiredLinkUnderTest):
    mono = wired_link.round_trip(
        encoded_audio_packet_bag(AnOpusPacketOffTheBroadcast(channels=1))
    )
    stereo = wired_link.round_trip(
        encoded_audio_packet_bag(AnOpusPacketOffTheBroadcast(channels=2))
    )

    assert EncodedAudioPacket(**mono).channels == 1
    assert EncodedAudioPacket(**stereo).channels == 2


def test_multichannel_opus_crosses_the_wire_whole(wired_link: WiredLinkUnderTest):
    """The `streamlib_bag` container carries what CMAF cannot: `dOps` encodes
    ChannelMappingFamily 0 only, so 3–8 channels have no representation there
    and are refused by name on that path rather than sent wrong."""
    bag = wired_link.round_trip(
        encoded_audio_packet_bag(AnOpusPacketOffTheBroadcast(channels=6))
    )

    assert EncodedAudioPacket(**bag).channels == 6
