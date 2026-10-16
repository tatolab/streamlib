# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The cast for an encoded-audio-packet message: one Opus packet plus its format.

The payload is a msgpack map carrying one Opus packet inline as msgpack
``bin``, so this cast composes nothing surface-shaped. *Packet*, not *frame*:
RFC 6716 §3 spends "frame" on a subdivision of one Opus packet. A producer
writes one with ``ctx.outputs.write(port, packet.to_bytes(), timestamp_ns=...)``
— the timestamped write, because a packet's stamp names its first sample's
instant and not the moment of publication.
"""

from __future__ import annotations

import typing
from dataclasses import dataclass, field
from typing import Any, Literal

from .msgpack import _MESSAGE_KEY_FIELD_METADATA, MsgpackCast

__all__ = ["EncodedAudioPacket"]

EncodedAudioCodec = Literal["opus"]

_REFUSAL_SUBJECT = "message is not an encoded audio packet"

_CODECS_ON_THE_WIRE: "tuple[str, ...]" = typing.get_args(EncodedAudioCodec)


def _is_plain_int(value: Any) -> bool:
    """`bool` is an `int` subclass; a sequence index of `True` is a bug."""
    return isinstance(value, int) and not isinstance(value, bool)


@dataclass(frozen=True, init=False)
class EncodedAudioPacket(MsgpackCast):
    """An encoded-audio-packet message, cast: one Opus packet plus its format.

    The packet's timestamp rides the message envelope, so a consumer that
    needs it reads ``ctx.inputs.read_with_timestamp(port)`` and casts the
    payload it hands back with ``EncodedAudioPacket.from_bytes``.

    ``sequence_index`` is monotonic in publication order for the life of the
    producer, so a step other than exactly one is loss. Every Opus packet is a
    sync point and its own group, so a consumer that sees a gap re-enters at
    the very next packet.

    Read through ``ctx.inputs.read(port, into=EncodedAudioPacket)``, which
    validates the message on construction.
    """

    codec: EncodedAudioCodec
    opus_packet_bytes: bytes = field(
        repr=False, metadata={_MESSAGE_KEY_FIELD_METADATA: "bitstream"}
    )
    is_sync_point: bool
    group_index: int
    sequence_index: int
    # Always 48 000: Opus's own clock, whatever the source was resampled from.
    sample_rate: int
    channels: int
    # Per-channel samples the packet spans — 960 at 20 ms framing.
    sample_count: int
    # The encoder's lookahead in 48 kHz samples: what a decoder discards at
    # entry, and what a container writes as the `OpusHead` PreSkip.
    pre_skip: int

    def __init__(
        self,
        codec: EncodedAudioCodec,
        bitstream: bytes,
        is_sync_point: bool,
        group_index: int,
        sequence_index: int,
        sample_rate: int,
        channels: int,
        sample_count: int,
        pre_skip: int,
        **keys_this_cast_does_not_read: Any,
    ) -> None:
        """Validate the message's entries and assign them.

        A message is an open map, so keys this cast does not read are ignored.
        """
        del keys_this_cast_does_not_read
        if not isinstance(bitstream, bytes):
            raise ValueError(
                f"{_REFUSAL_SUBJECT}: 'bitstream' must be bytes — one Opus "
                "packet riding the message as msgpack bin"
            )
        if codec not in _CODECS_ON_THE_WIRE:
            raise ValueError(
                f"{_REFUSAL_SUBJECT}: codec {codec!r} names no elementary stream "
                f"this cast reads ({', '.join(_CODECS_ON_THE_WIRE)})"
            )
        if not isinstance(is_sync_point, bool):
            raise ValueError(
                f"{_REFUSAL_SUBJECT}: 'is_sync_point' must be bool — it is the "
                "decode entry point, not a count"
            )
        integer_message_entries = (
            ("group_index", group_index),
            ("sequence_index", sequence_index),
            ("sample_rate", sample_rate),
            ("channels", channels),
            ("sample_count", sample_count),
            ("pre_skip", pre_skip),
        )
        mistyped = [
            key for key, value in integer_message_entries if not _is_plain_int(value)
        ]
        if mistyped:
            raise ValueError(
                f"{_REFUSAL_SUBJECT}: {mistyped[0]!r} must be int — "
                f"{', '.join(key for key, _ in integer_message_entries)} all are"
            )
        object.__setattr__(self, "codec", codec)
        object.__setattr__(self, "opus_packet_bytes", bitstream)
        object.__setattr__(self, "is_sync_point", is_sync_point)
        object.__setattr__(self, "group_index", group_index)
        object.__setattr__(self, "sequence_index", sequence_index)
        object.__setattr__(self, "sample_rate", sample_rate)
        object.__setattr__(self, "channels", channels)
        object.__setattr__(self, "sample_count", sample_count)
        object.__setattr__(self, "pre_skip", pre_skip)
