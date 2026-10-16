# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The cast for an encoded-video-frame message: one access unit plus how to place it.

The payload is a msgpack map carrying one Annex-B access unit inline as
msgpack ``bin``, so this cast composes nothing surface-shaped. A producer
writes one with ``ctx.outputs.write(port, frame.to_bytes(), timestamp_ns=...)``
— the timestamped write, because an encoded frame's stamp is the source
frame's and not the moment of publication.
"""

from __future__ import annotations

import typing
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Any, Literal

from .msgpack import _MESSAGE_KEY_FIELD_METADATA, MsgpackCast
from .video_frame import ColorInfo, _color_info_or_none

__all__ = ["EncodedVideoFrame"]

EncodedVideoCodec = Literal["h264", "h265"]

_REFUSAL_SUBJECT = "message is not an encoded video frame"

_CODECS_ON_THE_WIRE: "tuple[str, ...]" = typing.get_args(EncodedVideoCodec)


def _is_plain_int(value: Any) -> bool:
    """`bool` is an `int` subclass; a sequence index of `True` is a bug."""
    return isinstance(value, int) and not isinstance(value, bool)


@dataclass(frozen=True, init=False)
class EncodedVideoFrame(MsgpackCast):
    """An encoded-video-frame message, cast: one access unit plus how to place it.

    The frame's timestamp rides the message envelope, so a consumer that needs
    it reads ``ctx.inputs.read_with_timestamp(port)`` and casts the payload it
    hands back with ``EncodedVideoFrame.from_bytes``.

    ``sequence_index`` is monotonic in publication order for the life of the
    producer, so a step other than exactly one is loss; ``group_index`` counts
    sync points. A consumer that sees a gap discards until the next
    ``is_sync_point``, and enters a stream only at one.

    Read through ``ctx.inputs.read(port, into=EncodedVideoFrame)``, which
    validates the message on construction.
    """

    codec: EncodedVideoCodec
    annex_b_access_unit_bytes: bytes = field(
        repr=False, metadata={_MESSAGE_KEY_FIELD_METADATA: "bitstream"}
    )
    is_sync_point: bool
    group_index: int
    sequence_index: int
    # The coded extent, before the conformance crop: a 1080-line stream is
    # coded at 1088. The decoder's output carries the cropped extent.
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
        """Validate the message's entries and assign them.

        A message is an open map, so keys this cast does not read are ignored.
        """
        del keys_this_cast_does_not_read
        if not isinstance(bitstream, bytes):
            raise ValueError(
                f"{_REFUSAL_SUBJECT}: 'bitstream' must be bytes — one Annex-B "
                "access unit riding the message as msgpack bin"
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
            ("width", width),
            ("height", height),
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
        object.__setattr__(self, "annex_b_access_unit_bytes", bitstream)
        object.__setattr__(self, "is_sync_point", is_sync_point)
        object.__setattr__(self, "group_index", group_index)
        object.__setattr__(self, "sequence_index", sequence_index)
        object.__setattr__(self, "width", width)
        object.__setattr__(self, "height", height)
        object.__setattr__(
            self, "color", _color_info_or_none(_REFUSAL_SUBJECT, "color", color)
        )
