# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Stand-ins for what `_native.WhepSession.next_media` hands back, stated by a test."""

from dataclasses import dataclass

ANNEX_B_ACCESS_UNIT = b"\x00\x00\x00\x01\x67\x42\xe0\x1f\x00\x00\x00\x01\x65\x88\x84"
OPUS_PACKET = b"\x78\x01\x02\x03"

#: 37 microseconds as a one-byte QUIC varint (RFC 9000 §A.1) — the hang legacy
#: container's timestamp every media frame here opens with.
THIRTY_SEVEN_MICROSECONDS_AS_A_QUIC_VARINT = b"\x25"

MEDIA_FRAME_OF_AN_ACCESS_UNIT = THIRTY_SEVEN_MICROSECONDS_AS_A_QUIC_VARINT + ANNEX_B_ACCESS_UNIT
MEDIA_FRAME_OF_AN_OPUS_PACKET = THIRTY_SEVEN_MICROSECONDS_AS_A_QUIC_VARINT + OPUS_PACKET

#: A mono `OpusHead` (RFC 7845 §5.1): version 1, one channel, no pre-skip,
#: 48 kHz, no gain, mapping family 0.
MONO_OPUS_IDENTIFICATION_HEADER_HEX = "4f707573486561640101000080bb0000000000"


@dataclass(frozen=True)
class AccessUnitTheSessionPlayed:
    """A `_native.PlayedVideoAccessUnit` stand-in."""

    media_frame: bytes = MEDIA_FRAME_OF_AN_ACCESS_UNIT
    is_sync_point: bool = True
    codec: str = "avc1.42e01f"
    coded_width: int = 320
    coded_height: int = 192
    timestamp_ns: int = 37_000


@dataclass(frozen=True)
class OpusPacketTheSessionPlayed:
    """A `_native.PlayedOpusPacket` stand-in."""

    media_frame: bytes = MEDIA_FRAME_OF_AN_OPUS_PACKET
    channels: int = 1
    opus_identification_header_hex: str = MONO_OPUS_IDENTIFICATION_HEADER_HEX
    timestamp_ns: int = 37_000
