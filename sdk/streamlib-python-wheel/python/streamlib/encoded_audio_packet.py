# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The optional typed cast for an encoded-audio-packet bag.

A link carries a self-describing bag (a dict); its keys are the contract and
reading them directly is always enough. Casting is the opt-in dial for
consumers that want construction-time validation and attribute access instead
of key lookups.

Like an encoded video frame, an encoded audio packet carries its payload
inline: one Opus packet rides the bag as msgpack ``bin``, which reaches Python
as ``bytes``. So this cast composes nothing surface-shaped — there is no
surface id, no claim and no lifetime to hold.

*Packet*, not *frame*: RFC 6716 §3 spends the word "frame" on a subdivision of
one Opus packet, so a type named for the frame would mean two things at the
seam it crosses. One bag carries exactly one Opus packet.

This is the read side. Producing an encoded bag from Python is spelling these
keys as a bag literal against the wire contract and writing it with
``ctx.outputs.write(port, bag, timestamp_ns=...)`` — the timestamped write,
because a packet's stamp names its first sample's instant and not the moment of
publication. There is deliberately no to-bag helper here: a second spelling of
the contract is a second thing that can drift.
"""

from __future__ import annotations

import typing
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Any, Literal

__all__ = ["EncodedAudioPacket"]

EncodedAudioCodec = Literal["opus"]

# How every refusal from this cast names what the bag failed to be.
_REFUSAL_SUBJECT = "bag is not an encoded audio packet"

# The elementary streams this convention legalises, derived from the type
# rather than respelled so the two cannot drift. A `codec` naming anything
# else is refused rather than guessed at.
_CODECS_ON_THE_WIRE: "tuple[str, ...]" = typing.get_args(EncodedAudioCodec)

# Every key the convention carries; unlike the video cast's `color`, none of
# them is absent-means-unspecified, so this is the whole map.
_REQUIRED_BAG_KEYS = (
    "codec",
    "bitstream",
    "is_sync_point",
    "group_index",
    "sequence_index",
    "sample_rate",
    "channels",
    "sample_count",
    "pre_skip",
)


def _is_plain_int(value: Any) -> bool:
    """`bool` is an `int` subclass; a sequence index of `True` is a bug."""
    return isinstance(value, int) and not isinstance(value, bool)


@dataclass(frozen=True, init=False)
class EncodedAudioPacket:
    """An encoded-audio-packet bag, cast: one Opus packet plus its format.

    The packet's timestamp is not here — it rides the frame header like every
    bag's, so a consumer that needs it reads
    ``ctx.inputs.read_with_timestamp(port)`` and casts the bag it hands back.

    ``group_index`` and ``sequence_index`` are the same ordering pair an
    encoded video frame carries: ``sequence_index`` is monotonic in
    publication order for the life of the producer, so a step other than
    exactly one is loss and never a restart. Every Opus packet is a sync
    point — a decoder enters the stream at any of them — so every packet is
    its own group and a consumer that sees a gap re-enters at the very next
    packet rather than discarding until some later one.

    Read through ``ctx.inputs.read(port, into=EncodedAudioPacket)``, whose bag
    keys this validates on construction.
    """

    codec: EncodedAudioCodec
    # Off the repr: an Opus packet is up to 1275 bytes per stream, and a
    # failed assertion that prints all of them buries what it was about.
    opus_packet_bytes: bytes = field(repr=False)
    is_sync_point: bool
    group_index: int
    sequence_index: int
    # Always 48 000: Opus's own clock, the rate a decoder reconstructs at
    # whatever the source was resampled from.
    sample_rate: int
    channels: int
    # Per-channel samples the packet spans — 960 at the 20 ms framing this
    # convention uses, the unit `AudioBlock.sample_count` already counts in.
    sample_count: int
    # The encoder's lookahead in 48 kHz samples: what a decoder discards at
    # entry so its first emitted sample is the stamped instant, and what a
    # container writes as the `OpusHead` PreSkip.
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
        """Validate the bag's entries and assign them.

        Written out rather than generated because the bag is an open map: a
        producer may carry keys this cast does not read, and the day one adds
        a key must not be the day every
        `read(port, into=EncodedAudioPacket)` starts raising. Construction is
        the validation, so that spelling and `from_bag` are one path rather
        than two that can drift.
        """
        del keys_this_cast_does_not_read
        if not isinstance(bitstream, bytes):
            raise ValueError(
                f"{_REFUSAL_SUBJECT}: 'bitstream' must be bytes — one Opus "
                "packet riding the bag as msgpack bin"
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
        integer_bag_entries = (
            ("group_index", group_index),
            ("sequence_index", sequence_index),
            ("sample_rate", sample_rate),
            ("channels", channels),
            ("sample_count", sample_count),
            ("pre_skip", pre_skip),
        )
        mistyped = [key for key, value in integer_bag_entries if not _is_plain_int(value)]
        if mistyped:
            raise ValueError(
                f"{_REFUSAL_SUBJECT}: {mistyped[0]!r} must be int — "
                f"{', '.join(key for key, _ in integer_bag_entries)} all are"
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

    @classmethod
    def from_bag(cls, bag: Mapping[str, Any]) -> "EncodedAudioPacket":
        """Construct from a bag dict, raising ValueError on a missing or
        mistyped key.

        The same *validation* `read(port, into=EncodedAudioPacket)` performs,
        so the two spellings cannot disagree about what a valid encoded packet
        is; this one only names the missing key, which keyword construction
        reports in Python's words.
        """
        missing = [key for key in _REQUIRED_BAG_KEYS if key not in bag]
        if missing:
            raise ValueError(f"{_REFUSAL_SUBJECT}: missing key {missing[0]!r}")
        return cls(**bag)
