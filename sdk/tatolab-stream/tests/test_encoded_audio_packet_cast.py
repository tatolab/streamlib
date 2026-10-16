# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.EncodedAudioPacket` — the encoded-packet message's cast, read from a payload in hand.

What the cast refuses and how it reads its fields, decided before any wire is
involved.
"""

from typing import Any

import pytest

from tatolab.stream import EncodedAudioPacket
from tatolab.stream.msgpack import pack, unpack

# A stand-in Opus packet: a TOC byte and a few bytes of frame.
OPUS_PACKET = b"\x78\x01\x02\x03"

# The encoder's lookahead at 48 kHz — `Fs/400 + Fs/250`.
LOOKAHEAD_SAMPLES_AT_48_KHZ = 312

INTEGER_MESSAGE_KEYS = (
    "group_index",
    "sequence_index",
    "sample_rate",
    "channels",
    "sample_count",
    "pre_skip",
)

REQUIRED_MESSAGE_KEYS = ("codec", "bitstream", "is_sync_point", *INTEGER_MESSAGE_KEYS)


def encoded_packet_entries(**overrides: Any) -> "dict[str, Any]":
    """One 20 ms stereo Opus packet at the convention's own framing."""
    entries: "dict[str, Any]" = {
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
    entries.update(overrides)
    return entries


def encoded_packet_payload(**overrides: Any) -> bytes:
    return pack(encoded_packet_entries(**overrides))


def test_from_bytes_reads_the_opus_packet_as_bytes():
    packet = EncodedAudioPacket.from_bytes(encoded_packet_payload())

    assert packet.opus_packet_bytes == OPUS_PACKET
    assert (packet.sample_rate, packet.channels, packet.pre_skip) == (48_000, 2, 312)


def test_to_bytes_writes_the_bitstream_under_the_wire_key_and_round_trips():
    packet = EncodedAudioPacket.from_bytes(encoded_packet_payload())

    assert unpack(packet.to_bytes()) == encoded_packet_entries()
    assert EncodedAudioPacket.from_bytes(packet.to_bytes()) == packet


def test_a_bitstream_that_is_not_bytes_is_refused_by_name():
    with pytest.raises(ValueError, match="'bitstream' must be bytes"):
        EncodedAudioPacket.from_bytes(encoded_packet_payload(bitstream=[0x78, 0x01]))


def test_a_codec_naming_another_elementary_stream_is_refused_by_name():
    with pytest.raises(ValueError, match="codec 'vorbis' names no elementary stream"):
        EncodedAudioPacket.from_bytes(encoded_packet_payload(codec="vorbis"))


def test_a_sync_point_flag_that_is_not_a_bool_is_refused_by_name():
    with pytest.raises(ValueError, match="'is_sync_point' must be bool"):
        EncodedAudioPacket.from_bytes(encoded_packet_payload(is_sync_point=1))


@pytest.mark.parametrize("key", INTEGER_MESSAGE_KEYS)
def test_a_bool_is_refused_for_every_integer_field(key: str):
    with pytest.raises(ValueError, match=f"{key!r} must be int"):
        EncodedAudioPacket.from_bytes(encoded_packet_payload(**{key: True}))


@pytest.mark.parametrize("key", REQUIRED_MESSAGE_KEYS)
def test_a_missing_key_is_named(key: str):
    entries = encoded_packet_entries()
    del entries[key]

    with pytest.raises(TypeError, match=key):
        EncodedAudioPacket.from_bytes(pack(entries))


def test_an_encoded_audio_packet_takes_no_surface_and_holds_no_claim():
    packet = EncodedAudioPacket.from_bytes(encoded_packet_payload())

    assert not hasattr(packet, "surface_id")
    assert not hasattr(packet, "writable")
    assert not hasattr(packet, "__dlpack__")


def test_the_packet_payload_stays_off_the_repr():
    rendered = repr(EncodedAudioPacket.from_bytes(encoded_packet_payload()))

    assert "sequence_index=0" in rendered
    assert "opus_packet_bytes" not in rendered
