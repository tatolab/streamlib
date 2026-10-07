# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.EncodedAudioPacket` — the encoded bag's cast, built from a bag in hand.

What the cast refuses and how it reads its fields, decided before any wire is
involved. That the payload survives a real link as `bytes` is tested against
the runtime.
"""

from typing import Any

import pytest

from tatolab.stream import EncodedAudioPacket
from tatolab.stream.encoded_audio_packet import _REQUIRED_BAG_KEYS


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


def test_a_bitstream_that_is_not_bytes_is_refused_by_name():
    """The mistake this catches is the one that otherwise decodes silently: a
    producer whose Opus packet went out as a list of numbers rather than a
    buffer."""
    with pytest.raises(ValueError, match="'bitstream' must be bytes"):
        EncodedAudioPacket.from_bag(encoded_packet_bag(bitstream=[0x78, 0x01]))


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
