# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Stand-ins for what `_native.WhepSession.next_media` hands back, stated by a test."""

from dataclasses import dataclass

ANNEX_B_ACCESS_UNIT = b"\x00\x00\x00\x01\x67\x42\xe0\x1f\x00\x00\x00\x01\x65\x88\x84"
OPUS_PACKET = b"\x78\x01\x02\x03"


@dataclass(frozen=True)
class AccessUnitTheSessionPlayed:
    """A `_native.PlayedVideoAccessUnit` stand-in."""

    bitstream: bytes = ANNEX_B_ACCESS_UNIT
    is_sync_point: bool = True
    group_index: int = 3
    sequence_index: int = 11
    width: int = 320
    height: int = 180
    color: "dict[str, str] | None" = None
    timestamp_ns: int = 0


@dataclass(frozen=True)
class OpusPacketTheSessionPlayed:
    """A `_native.PlayedOpusPacket` stand-in."""

    bitstream: bytes = OPUS_PACKET
    group_index: int = 7
    sequence_index: int = 7
    sample_rate: int = 48_000
    channels: int = 1
    sample_count: int = 960
    pre_skip: int = 0
    timestamp_ns: int = 0
