# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The cast for an audio-block message: interleaved CPU samples plus how to read them.

The payload is the msgpack map the microphone writes. The samples ride it
inline as msgpack ``bin`` and ``dtype`` says how to read them, so this cast
composes nothing surface-shaped. ``samples`` is a numpy view over the
message's own ``bytes``, adding no copy of the payload.
"""

from __future__ import annotations

import typing
from dataclasses import dataclass, field
from typing import Any, Literal

from .msgpack import _MESSAGE_KEY_FIELD_METADATA, MsgpackCast

if typing.TYPE_CHECKING:
    from numpy.typing import NDArray

__all__ = ["AudioBlock"]

AudioSampleDtype = Literal["f32", "i16"]

# The payload is little-endian by contract, so the numpy spelling is
# little-endian and never the platform's native one.
_NUMPY_TYPE_FOR_DTYPE: "dict[str, str]" = {"f32": "<f4", "i16": "<i2"}
_BYTES_PER_SAMPLE_FOR_DTYPE: "dict[str, int]" = {"f32": 4, "i16": 2}

_AUDIO_BLOCK_REFUSAL_SUBJECT = "message is not an audio block"


def _is_plain_int(value: Any) -> bool:
    """`bool` is an `int` subclass; a channel count of `True` is a bug."""
    return isinstance(value, int) and not isinstance(value, bool)


@dataclass(frozen=True, init=False)
class AudioBlock(MsgpackCast):
    """An audio-block message, cast: interleaved CPU samples plus how to read them.

    ``first_sample_timestamp_ns`` (the machine's monotonic clock in
    nanoseconds) is the whole of A/V sync: any sample's instant derives from
    it, ``sample_count`` and ``sample_rate``, so joining a block to a camera
    frame is subtracting two timestamps.

    Read through ``ctx.inputs.read(port, into=AudioBlock)``, which validates
    the message on construction; ``to_bytes()`` writes it back.
    """

    interleaved_sample_bytes: bytes = field(
        repr=False, metadata={_MESSAGE_KEY_FIELD_METADATA: "samples"}
    )
    sample_rate: int
    channels: int
    sample_count: int
    dtype: AudioSampleDtype
    first_sample_timestamp_ns: int

    def __init__(
        self,
        samples: bytes,
        sample_rate: int,
        channels: int,
        sample_count: int,
        first_sample_timestamp_ns: int,
        dtype: AudioSampleDtype = "f32",
        **keys_this_cast_does_not_read: Any,
    ) -> None:
        """Validate the message's entries and assign them.

        A message is an open map, so keys this cast does not read are ignored.
        """
        del keys_this_cast_does_not_read
        if not isinstance(samples, bytes):
            raise ValueError(
                f"{_AUDIO_BLOCK_REFUSAL_SUBJECT}: 'samples' must be bytes — the "
                "samples ride the message as msgpack bin"
            )
        if (
            not _is_plain_int(sample_rate)
            or not _is_plain_int(channels)
            or not _is_plain_int(sample_count)
            or not _is_plain_int(first_sample_timestamp_ns)
        ):
            raise ValueError(
                f"{_AUDIO_BLOCK_REFUSAL_SUBJECT}: sample_rate/channels/sample_count/"
                "first_sample_timestamp_ns must be int"
            )
        # Before the length check, which two negatives would pass by cancelling.
        if sample_count < 0 or channels < 0:
            raise ValueError(
                f"{_AUDIO_BLOCK_REFUSAL_SUBJECT}: sample_count={sample_count} and "
                f"channels={channels} must both be non-negative"
            )
        if dtype not in _NUMPY_TYPE_FOR_DTYPE:
            raise ValueError(
                f"{_AUDIO_BLOCK_REFUSAL_SUBJECT}: dtype {dtype!r} is not one this "
                f"cast reads ({', '.join(sorted(_NUMPY_TYPE_FOR_DTYPE))})"
            )
        # `reshape` alone would accept a wrong length that happens to divide.
        bytes_per_sample = _BYTES_PER_SAMPLE_FOR_DTYPE[dtype]
        expected_byte_count = sample_count * channels * bytes_per_sample
        if len(samples) != expected_byte_count:
            raise ValueError(
                f"{_AUDIO_BLOCK_REFUSAL_SUBJECT}: 'samples' carries {len(samples)} "
                f"bytes, but sample_count={sample_count} × channels={channels} × "
                f"{bytes_per_sample} bytes per {dtype} sample is {expected_byte_count}"
            )
        object.__setattr__(self, "interleaved_sample_bytes", samples)
        object.__setattr__(self, "sample_rate", sample_rate)
        object.__setattr__(self, "channels", channels)
        object.__setattr__(self, "sample_count", sample_count)
        object.__setattr__(self, "dtype", dtype)
        object.__setattr__(
            self, "first_sample_timestamp_ns", first_sample_timestamp_ns
        )

    @property
    def samples(self) -> "NDArray[Any]":
        """The samples as a read-only ``(sample_count, channels)`` numpy view.

        Take a copy (``numpy.array(block.samples)``) to write into it.
        """
        # Imported here so the package takes no numpy dependency.
        import numpy

        return numpy.frombuffer(
            self.interleaved_sample_bytes, dtype=_NUMPY_TYPE_FOR_DTYPE[self.dtype]
        ).reshape(self.sample_count, self.channels)
