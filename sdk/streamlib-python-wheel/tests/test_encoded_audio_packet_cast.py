# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.EncodedAudioPacket` — the encoded record's cast, over a live link.

An encoded packet carries its payload inline, so what has to survive the wire
is the payload's msgpack *type*: `bin`, which unpacks as `bytes`. That
is what these tests drive, over real wired iceoryx2 ports rather than a
stand-in, because a message has to survive the wire before `into=` has anything to
cast.

Both ends live on this thread because iceoryx2's ports are `!Send`, and the
destination is wired first because a send with no subscriber attached is
dropped. No engine and no GPU: the ports are wired directly, which is what
keeps this half of the proof in CI. The half that needs the real library —
packets libopus actually wrote — is `test_opus_blocks.py`.
"""

import os
from collections.abc import Iterator
from typing import Any

import pytest

from tatolab.runtime import _engine
from tatolab.stream import EncodedAudioPacket, NodeLinkDataAccess, msgpack
from tatolab.stream.encoded_audio_packet import _CODECS_ON_THE_WIRE

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)

OUTPUT_PORT = "encoded_audio_to_downstream"
INPUT_PORT = "encoded_audio_from_upstream"

# A stand-in Opus packet: a TOC byte and a few bytes of frame. Short on
# purpose — nothing here decodes it, and what is under test is that the bytes
# arrive as bytes.
OPUS_PACKET = b"\x78\x01\x02\x03"

# The encoder's lookahead at 48 kHz — `Fs/400 + Fs/250` — which is what a
# decoder trims at entry and what a container writes as PreSkip.
LOOKAHEAD_SAMPLES_AT_48_KHZ = 312


def encoded_packet_record(**overrides: Any) -> "dict[str, Any]":
    """One 20 ms stereo Opus packet at the convention's own framing."""
    record: "dict[str, Any]" = {
        "codec": "opus",
        "bitstream": OPUS_PACKET,
        "is_sync_point": True,
        "group_index": 0,
        "sequence_index": 0,
        "sample_rate": 48_000,
        "channels": 2,
        "sample_count": 960,
        "pre_skip": LOOKAHEAD_SAMPLES_AT_48_KHZ,
    }
    record.update(overrides)
    return record


class WiredLinkUnderTest:
    """One live link, from the writing end to the reading end."""

    def __init__(
        self, source: NodeLinkDataAccess, destination: NodeLinkDataAccess
    ) -> None:
        self.source = source
        self.destination = destination

    def deliver(self, record: "dict[str, Any]") -> None:
        self.source.write_to_output_port(OUTPUT_PORT, msgpack.pack(record))


@pytest.fixture
def wired_link(request: pytest.FixtureRequest) -> Iterator[WiredLinkUnderTest]:
    """A source and a destination joined by one link.

    Service names carry the test's own name because every test in this
    process shares one iceoryx2 domain — a fixed name would let one test's
    channel meet the next one's.
    """
    unique = f"encaud{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/encoded_audio"
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
    wire, so the unpacked record holds `bytes` rather than a list of numbers.

    A producer whose packet encoded as a msgpack array would still read back
    equal-looking data here — as a `list`, five bytes on the wire per byte of
    audio, and unreadable as a buffer by a muxer, a socket, or a consumer in
    another language.
    """
    wired_link.deliver(encoded_packet_record())

    payload = wired_link.destination.read_from_input_port(INPUT_PORT)

    assert payload is not None
    record = msgpack.unpack(payload)
    assert type(record["bitstream"]) is bytes
    assert record["bitstream"] == OPUS_PACKET


def test_every_wire_key_survives_the_read_into_an_encoded_audio_packet(
    wired_link: WiredLinkUnderTest,
):
    """All nine keys, off a live link, in one assertion each — this is the
    lock on the wire contract the Opus built-ins publish against."""
    wired_link.deliver(
        encoded_packet_record(group_index=7, sequence_index=7, channels=6)
    )

    packet = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=EncodedAudioPacket
    )

    assert isinstance(packet, EncodedAudioPacket)
    assert packet.codec == "opus"
    assert packet.opus_packet_bytes == OPUS_PACKET
    assert packet.is_sync_point is True
    assert packet.group_index == 7
    assert packet.sequence_index == 7
    assert packet.sample_rate == 48_000
    assert packet.channels == 6
    assert packet.sample_count == 960
    assert packet.pre_skip == LOOKAHEAD_SAMPLES_AT_48_KHZ


def test_a_key_this_cast_does_not_read_does_not_break_the_read(
    wired_link: WiredLinkUnderTest,
):
    """The record is an open map: the day a producer adds a key must not be the day
    every `read(port, into=EncodedAudioPacket)` starts raising."""
    wired_link.deliver(encoded_packet_record(a_future_key="ignored"))

    packet = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=EncodedAudioPacket
    )

    assert isinstance(packet, EncodedAudioPacket)
    assert packet.sequence_index == 0
    assert not hasattr(packet, "a_future_key")


@pytest.mark.parametrize("codec", _CODECS_ON_THE_WIRE)
def test_the_codec_this_convention_legalises_reads_through_the_cast(
    wired_link: WiredLinkUnderTest, codec: str
):
    """Parametrized over the type's own arguments rather than the string, so
    the day the convention legalises a second codec this test covers it
    without being edited."""
    wired_link.deliver(encoded_packet_record(codec=codec))

    packet = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=EncodedAudioPacket
    )

    assert isinstance(packet, EncodedAudioPacket)
    assert packet.codec == codec
