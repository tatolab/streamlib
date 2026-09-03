# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`streamlib.EncodedAudioPacket` — the encoded bag's cast, over a live link.

An encoded packet carries its payload inline, so what has to survive the wire
is the payload's msgpack *type*: `bin`, which reaches Python as `bytes`. That
is what these tests drive, over real wired iceoryx2 ports rather than a
stand-in, because a bag has to survive the wire before `into=` has anything to
cast.

Both ends live on this thread because iceoryx2's ports are `!Send`, and the
destination is wired first because a send with no subscriber attached is
dropped. No engine and no GPU: the ports are wired directly, which is what
keeps this half of the proof in CI. The half that needs the real library —
bags libopus actually wrote — is `test_opus_blocks.py`.
"""

import os
from collections.abc import Iterator
from typing import Any

import pytest

from streamlib import EncodedAudioPacket, ProcessorLinkDataAccess
from streamlib.encoded_audio_packet import _CODECS_ON_THE_WIRE, _REQUIRED_BAG_KEYS

OUTPUT_PORT = "encoded_audio_to_downstream"
INPUT_PORT = "encoded_audio_from_upstream"

# A stand-in Opus packet: a TOC byte and a few bytes of frame. Short on
# purpose — nothing here decodes it, and what is under test is that the bytes
# arrive as bytes.
OPUS_PACKET = b"\x78\x01\x02\x03"

# The encoder's lookahead at 48 kHz — `Fs/400 + Fs/250` — which is what a
# decoder trims at entry and what a container writes as PreSkip.
LOOKAHEAD_SAMPLES_AT_48_KHZ = 312

# Every integer the convention carries. A `bool` in any of them would arrive
# as 0 or 1 and read as a plausible value, which is what makes the whole set
# worth naming rather than the ordering pair alone.
INTEGER_BAG_KEYS = (
    "group_index",
    "sequence_index",
    "sample_rate",
    "channels",
    "sample_count",
    "pre_skip",
)


def encoded_packet_bag(**overrides: Any) -> "dict[str, Any]":
    """One 20 ms stereo Opus packet at the convention's own framing."""
    bag: "dict[str, Any]" = {
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
    bag.update(overrides)
    return bag


class WiredLinkUnderTest:
    """One live link, from the writing end to the reading end."""

    def __init__(
        self, source: ProcessorLinkDataAccess, destination: ProcessorLinkDataAccess
    ) -> None:
        self.source = source
        self.destination = destination

    def deliver(self, bag: "dict[str, Any]") -> None:
        self.source.write_to_output_port(OUTPUT_PORT, bag)


@pytest.fixture
def wired_link(request: pytest.FixtureRequest) -> Iterator[WiredLinkUnderTest]:
    """A source and a destination joined by one link.

    Service names carry the pid and the test's own name because iceoryx2
    service state is machine-global and outlives a crashed process — a fixed
    name would let one bad run poison every later one.
    """
    unique = f"encaud{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/encoded_audio"
    notify_service_name = f"{unique}_dest/notify"
    link_id = f"L-{unique}"

    destination = ProcessorLinkDataAccess()
    destination.wire_input_link(
        INPUT_PORT,
        channel_service_name,
        notify_service_name,
        "read_next_in_order",
        8,
        2,
        1,
        link_id,
    )
    source = ProcessorLinkDataAccess()
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

    A producer whose packet encoded as a msgpack array would still read back
    equal-looking data here — as a `list`, five bytes on the wire per byte of
    audio, and unreadable as a buffer by a muxer, a socket, or a consumer in
    another language.
    """
    wired_link.deliver(encoded_packet_bag())

    bag = wired_link.destination.read_from_input_port(INPUT_PORT)

    assert bag is not None
    assert type(bag["bitstream"]) is bytes
    assert bag["bitstream"] == OPUS_PACKET


def test_every_wire_key_survives_the_read_into_an_encoded_audio_packet(
    wired_link: WiredLinkUnderTest,
):
    """All nine keys, off a live link, in one assertion each — this is the
    lock on the wire contract the Opus built-ins publish against."""
    wired_link.deliver(
        encoded_packet_bag(group_index=7, sequence_index=7, channels=6)
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
    """The bag map is open: the day a producer adds a key must not be the day
    every `read(port, into=EncodedAudioPacket)` starts raising."""
    wired_link.deliver(encoded_packet_bag(a_future_key="ignored"))

    packet = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=EncodedAudioPacket
    )

    assert isinstance(packet, EncodedAudioPacket)
    assert packet.sequence_index == 0
    assert not hasattr(packet, "a_future_key")


def test_a_bitstream_that_is_not_bytes_is_refused_by_name():
    """The mistake this catches is the one that otherwise decodes silently: a
    producer whose Opus packet went out as a list of numbers rather than a
    buffer."""
    with pytest.raises(ValueError, match="'bitstream' must be bytes"):
        EncodedAudioPacket.from_bag(encoded_packet_bag(bitstream=[0x78, 0x01]))


@pytest.mark.parametrize("codec", _CODECS_ON_THE_WIRE)
def test_the_codec_this_convention_legalises_reads_through_the_cast(
    wired_link: WiredLinkUnderTest, codec: str
):
    """Parametrized over the type's own arguments rather than the string, so
    the day the convention legalises a second codec this test covers it
    without being edited."""
    wired_link.deliver(encoded_packet_bag(codec=codec))

    packet = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=EncodedAudioPacket
    )

    assert isinstance(packet, EncodedAudioPacket)
    assert packet.codec == codec


def test_a_codec_naming_another_elementary_stream_is_refused_by_name():
    with pytest.raises(ValueError, match="codec 'vorbis' names no elementary stream"):
        EncodedAudioPacket.from_bag(encoded_packet_bag(codec="vorbis"))


def test_a_sync_point_flag_that_is_not_a_bool_is_refused_by_name():
    """Every Opus packet is a sync point, so this flag is a constant of the
    convention — but a truthy `1` standing in for it is still a producer bug,
    and the door stays shut on the codec whose packets are not all sync
    points."""
    with pytest.raises(ValueError, match="'is_sync_point' must be bool"):
        EncodedAudioPacket.from_bag(encoded_packet_bag(is_sync_point=1))


@pytest.mark.parametrize("key", INTEGER_BAG_KEYS)
def test_a_bool_is_refused_for_every_integer_field(key: str):
    """`bool` is an `int` subclass, so a `channels` of `True` would otherwise
    arrive as 1 and read as a plausible mono stream."""
    with pytest.raises(ValueError, match=f"{key!r} must be int"):
        EncodedAudioPacket.from_bag(encoded_packet_bag(**{key: True}))


@pytest.mark.parametrize("key", _REQUIRED_BAG_KEYS)
def test_a_missing_key_is_named(key: str):
    """No key of this convention is absent-means-unspecified — unlike the
    video cast's `color` — so all nine are named when they go missing."""
    bag = encoded_packet_bag()
    del bag[key]

    with pytest.raises(ValueError, match=f"missing key {key!r}"):
        EncodedAudioPacket.from_bag(bag)


def test_an_encoded_audio_packet_takes_no_surface_and_holds_no_claim():
    """An Opus packet touches no surface machinery at all — the cast composes
    nothing that would demand a surface id or take a claim, which is why a
    packet is constructible from a bag this test wrote by hand."""
    packet = EncodedAudioPacket.from_bag(encoded_packet_bag())

    assert not hasattr(packet, "surface_id")
    assert not hasattr(packet, "writable")
    assert not hasattr(packet, "__dlpack__")


def test_the_packet_payload_stays_off_the_repr():
    """A failed assertion on an encoded packet prints its ordering, not its
    compressed audio."""
    rendered = repr(EncodedAudioPacket.from_bag(encoded_packet_bag()))

    assert "sequence_index=0" in rendered
    assert "opus_packet_bytes" not in rendered


def test_the_cast_offers_no_way_back_onto_the_wire():
    """Producing an encoded bag is spelling the keys against the wire
    contract and writing it with `ctx.outputs.write(port, bag,
    timestamp_ns=...)`. A to-bag helper would be a second spelling of the
    contract, and `dataclasses.asdict` would emit `opus_packet_bytes` rather
    than the wire's `bitstream`."""
    packet = EncodedAudioPacket.from_bag(encoded_packet_bag())

    assert not hasattr(packet, "to_bag")
    assert not hasattr(packet, "as_bag")
