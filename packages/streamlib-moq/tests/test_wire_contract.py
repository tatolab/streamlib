# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What `MoqBroadcastSubscriber` writes, read back as what a decoder casts it to
— and, for a data track, read back as the bag its producer wrote.

The bags go over real iceoryx2 links rather than being compared as dicts,
because the thing worth pinning is that every key survives the wire — that
`bitstream` arrives as `bytes` and not as a list of integers, that the
engine's own casts accept the map without a key missing, and that a data
bag's `bytes` value is still `bytes` under the producer's own stamp.

GPU-free: the links are wired directly and no runtime is started.
"""

import os
from collections.abc import Iterator, Mapping
from dataclasses import dataclass
from typing import Any
from unittest import mock

import pytest

from streamlib import EncodedAudioPacket, EncodedVideoFrame, encode_bag_to_msgpack_bytes, log
from streamlib._engine import ProcessorLinkDataAccess
from streamlib_moq import MoqBroadcastSubscriber, _native
from streamlib_moq.processors import (
    DATA_BAGS_OUTPUT_PORT,
    encoded_audio_packet_bag,
    encoded_video_frame_bag,
)

INPUT_PORT = "encoded_in"
OUTPUT_PORT = "encoded_out"

ANNEX_B_ACCESS_UNIT = b"\x00\x00\x00\x01\x67\x42\xe0\x1f\x00\x00\x00\x01\x65\x88\x84"
OPUS_PACKET = b"\x78\x01\x02\x03"

DATA_TRACK = "telemetry"

#: The data object's shape as the change file fixes it. The publisher writes it
#: and the subscriber reads it, and the two meet only on the wire — so these
#: bytes are built here from the documented shape, never from the publisher's
#: own code, and a drift on either side is ticket 4's end-to-end run to catch.
THE_DOCUMENTED_ENVELOPE: "dict[str, Any]" = {
    "sequence_index": 7,
    "timestamp_ns": 123,
    "bag": {"a": b"x", "n": {"k": [1, 2.5, None]}},
}


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


def a_data_object_off_the_broadcast(payload: bytes) -> _native.ReceivedDataObject:
    """What the native layer hands the data path: the object's bytes, whole."""
    return _native.ReceivedDataObject(DATA_TRACK, payload)


class OutputsWritingOverTheWiredLink:
    """`ctx.outputs` as the subscriber's data path sees it, over the fixture's
    one link — so the write, stamp included, is the engine's own."""

    def __init__(self, source: ProcessorLinkDataAccess) -> None:
        self._source = source
        self.ports_written: "list[str]" = []

    def write(
        self, port: str, bag: Mapping[str, Any], timestamp_ns: "int | None" = None
    ) -> None:
        self.ports_written.append(port)
        self._source.write_to_output_port(OUTPUT_PORT, bag, timestamp_ns)


def a_data_track_subscriber() -> MoqBroadcastSubscriber:
    return MoqBroadcastSubscriber(
        relay_url="https://relay.invalid/a-token",
        broadcast="a-broadcast",
        container_format="streamlib_bag",
        data_track=DATA_TRACK,
    )


def an_envelope_stating(**overrides: Any) -> bytes:
    return encode_bag_to_msgpack_bytes({**THE_DOCUMENTED_ENVELOPE, **overrides})


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


def test_a_data_object_reaches_the_reader_as_the_bag_its_producer_wrote_under_its_stamp(
    wired_link: WiredLinkUnderTest,
):
    """The bag verbatim, `bytes` still `bytes`, the producer's `timestamp_ns`
    on the frame header — and neither envelope key inside the bag."""
    subscriber = a_data_track_subscriber()
    outputs = OutputsWritingOverTheWiredLink(wired_link.source)

    subscriber._write_a_data_object(
        a_data_object_off_the_broadcast(an_envelope_stating()),
        outputs,  # type: ignore[arg-type]
    )
    bag, timestamp_ns = wired_link.destination.read_from_input_port_with_timestamp(
        INPUT_PORT
    )
    assert bag is not None, "the wired input received nothing"

    assert outputs.ports_written == [DATA_BAGS_OUTPUT_PORT]
    assert bag == THE_DOCUMENTED_ENVELOPE["bag"]
    assert type(bag["a"]) is bytes
    assert timestamp_ns == 123
    assert "sequence_index" not in bag and "timestamp_ns" not in bag


def test_an_object_missing_an_envelope_key_is_refused_by_name_and_nothing_is_written(
    wired_link: WiredLinkUnderTest,
):
    subscriber = a_data_track_subscriber()
    outputs = OutputsWritingOverTheWiredLink(wired_link.source)
    said: "list[str]" = []

    with mock.patch.object(log, "warn", said.append):
        subscriber._write_a_data_object(
            a_data_object_off_the_broadcast(
                encode_bag_to_msgpack_bytes({"sequence_index": 7, "bag": {"a": b"x"}})
            ),
            outputs,  # type: ignore[arg-type]
        )

    assert outputs.ports_written == []
    assert wired_link.destination.read_from_input_port(INPUT_PORT) is None
    assert len(said) == 1, said
    assert "`timestamp_ns`" in said[0] and f"`{DATA_TRACK}`" in said[0], said[0]


def test_a_jump_in_the_sequence_index_is_counted_while_every_bag_still_reaches_the_reader(
    wired_link: WiredLinkUnderTest,
):
    """A gap is said, never raised: the engine offers no lossless link, and a
    subscriber that stopped writing on one would turn a loss into an outage."""
    subscriber = a_data_track_subscriber()
    outputs = OutputsWritingOverTheWiredLink(wired_link.source)

    for sequence_index in (3, 4, 9):
        subscriber._write_a_data_object(
            a_data_object_off_the_broadcast(
                an_envelope_stating(sequence_index=sequence_index, bag={"n": sequence_index})
            ),
            outputs,  # type: ignore[arg-type]
        )

    assert [wired_link.destination.read_from_input_port(INPUT_PORT) for _ in range(3)] == [
        {"n": 3},
        {"n": 4},
        {"n": 9},
    ]
    assert (
        subscriber._data_sequence_gaps.gaps,
        subscriber._data_sequence_gaps.objects_missed,
    ) == (1, 4)



def test_a_bag_with_a_key_the_engine_cannot_write_is_refused_by_name_before_the_write(
    wired_link: WiredLinkUnderTest,
):
    """Wire-legal msgpack a non-StreamLib publisher can send: an int key in a
    nested map, which the engine's decoder accepts and its encoder refuses at
    the write. Refused before the write, so one object cannot end the
    subscription and take the media ports with it."""
    subscriber = a_data_track_subscriber()
    outputs = OutputsWritingOverTheWiredLink(wired_link.source)
    # {"sequence_index": 1, "timestamp_ns": 2, "bag": {"nested": {1: "x"}}},
    # spelled by hand because the engine's encoder will not write it.
    wire = (
        b"\x83\xaesequence_index\x01\xactimestamp_ns\x02\xa3bag"
        b"\x81\xa6nested\x81\x01\xa1x"
    )
    said: "list[str]" = []

    with mock.patch.object(log, "warn", said.append):
        subscriber._write_a_data_object(
            a_data_object_off_the_broadcast(wire),
            outputs,  # type: ignore[arg-type]
        )

    assert outputs.ports_written == []
    assert wired_link.destination.read_from_input_port(INPUT_PORT) is None
    assert len(said) == 1 and "not a str at `bag.nested`: 1 (int)" in said[0], said
