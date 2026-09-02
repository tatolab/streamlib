# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The optional typed cast for an encoded-video-frame bag.

A link carries a self-describing bag (a dict); its keys are the contract and
reading them directly is always enough. Casting is the opt-in dial for
consumers that want construction-time validation and attribute access instead
of key lookups.

Like an audio block and unlike a video frame, an encoded frame carries its
payload inline: one Annex-B access unit rides the bag as msgpack ``bin``,
which reaches Python as ``bytes``. So this cast composes nothing
surface-shaped — there is no surface id, no claim and no lifetime to hold.

This is the read side. Producing an encoded bag from Python is spelling these
keys as a bag literal against the wire contract and writing it with
``ctx.outputs.write(port, bag, timestamp_ns=...)`` — the timestamped write,
because an encoded frame's stamp is the source frame's and not the moment of
publication. There is deliberately no to-bag helper here: a second spelling of
the contract is a second thing that can drift.
"""

from __future__ import annotations

import typing
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Any, Literal

from .video_frame import ColorInfo, _color_info_or_none

__all__ = ["EncodedVideoFrame"]

EncodedVideoCodec = Literal["h264", "h265"]

# How every refusal from this cast names what the bag failed to be.
_REFUSAL_SUBJECT = "bag is not an encoded video frame"

# The elementary streams this convention legalises, derived from the type
# rather than respelled so the two cannot drift. A `codec` naming anything
# else is refused rather than guessed at.
_CODECS_ON_THE_WIRE: "tuple[str, ...]" = typing.get_args(EncodedVideoCodec)

# The keys without which there is no encoded frame to speak of. `color` is
# absent-means-unspecified, so it is not among them; everything else the bag
# carries is optional, this cast's or somebody else's.
_REQUIRED_BAG_KEYS = (
    "codec",
    "bitstream",
    "is_sync_point",
    "group_index",
    "sequence_index",
    "width",
    "height",
)


def _is_plain_int(value: Any) -> bool:
    """`bool` is an `int` subclass; a sequence index of `True` is a bug."""
    return isinstance(value, int) and not isinstance(value, bool)


@dataclass(frozen=True, init=False)
class EncodedVideoFrame:
    """An encoded-video-frame bag, cast: one access unit plus how to place it.

    The frame's timestamp is not here — it rides the frame header like every
    bag's, so a consumer that needs it reads
    ``ctx.inputs.read_with_timestamp(port)`` and casts the bag it hands back.

    ``group_index`` and ``sequence_index`` are the ordering pair:
    ``sequence_index`` is monotonic in publication order for the life of the
    producer, so a step other than exactly one is loss and never a restart,
    and ``group_index`` counts sync points. A consumer that sees a gap
    discards until the next ``is_sync_point``, and enters a stream only at
    one — the first bag off a link is not necessarily the producer's first.

    Read through ``ctx.inputs.read(port, into=EncodedVideoFrame)``, whose bag
    keys this validates on construction.
    """

    codec: EncodedVideoCodec
    # Off the repr: an access unit is tens of kilobytes, and a failed
    # assertion that prints all of them buries what it was actually about.
    annex_b_access_unit_bytes: bytes = field(repr=False)
    is_sync_point: bool
    group_index: int
    sequence_index: int
    # The *coded* extent, before the conformance crop: both codecs pad up to
    # a block size, so a 1080-line stream is coded at 1088 and these are the
    # padded numbers. The decoder's own output carries the cropped extent.
    width: int
    height: int
    color: ColorInfo | None = None

    def __init__(
        self,
        codec: EncodedVideoCodec,
        bitstream: bytes,
        is_sync_point: bool,
        group_index: int,
        sequence_index: int,
        width: int,
        height: int,
        color: "ColorInfo | Mapping[str, Any] | None" = None,
        **keys_this_cast_does_not_read: Any,
    ) -> None:
        """Validate the bag's entries and assign them.

        Written out rather than generated because the bag is an open map: a
        producer may carry keys this cast does not read, and the day one adds
        a key must not be the day every
        `read(port, into=EncodedVideoFrame)` starts raising. Construction is
        the validation, so that spelling and `from_bag` are one path rather
        than two that can drift.
        """
        del keys_this_cast_does_not_read
        if not isinstance(bitstream, bytes):
            raise ValueError(
                f"{_REFUSAL_SUBJECT}: 'bitstream' must be bytes — one Annex-B "
                "access unit riding the bag as msgpack bin"
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
            ("width", width),
            ("height", height),
        )
        mistyped = [key for key, value in integer_bag_entries if not _is_plain_int(value)]
        if mistyped:
            raise ValueError(
                f"{_REFUSAL_SUBJECT}: {mistyped[0]!r} must be int — "
                f"{', '.join(key for key, _ in integer_bag_entries)} all are"
            )
        object.__setattr__(self, "codec", codec)
        object.__setattr__(self, "annex_b_access_unit_bytes", bitstream)
        object.__setattr__(self, "is_sync_point", is_sync_point)
        object.__setattr__(self, "group_index", group_index)
        object.__setattr__(self, "sequence_index", sequence_index)
        object.__setattr__(self, "width", width)
        object.__setattr__(self, "height", height)
        # The same H.273 four-tuple a video-frame bag carries as `color_info`,
        # read by the same field-by-field reader under this convention's key.
        object.__setattr__(
            self, "color", _color_info_or_none(_REFUSAL_SUBJECT, "color", color)
        )

    @classmethod
    def from_bag(cls, bag: Mapping[str, Any]) -> "EncodedVideoFrame":
        """Construct from a bag dict, raising ValueError on a missing or
        mistyped key.

        The same *validation* `read(port, into=EncodedVideoFrame)` performs,
        so the two spellings cannot disagree about what a valid encoded frame
        is; this one only names the missing key, which keyword construction
        reports in Python's words.
        """
        missing = [key for key in _REQUIRED_BAG_KEYS if key not in bag]
        if missing:
            raise ValueError(f"{_REFUSAL_SUBJECT}: missing key {missing[0]!r}")
        return cls(**bag)
