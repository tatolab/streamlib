# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The optional typed cast for an audio-block bag.

A link carries a self-describing bag (a dict); its keys are the contract and
reading them directly is always enough. Casting is the opt-in dial for
consumers that want construction-time validation and attribute access instead
of key lookups.

Unlike a video frame, an audio block carries its payload inline: the samples
ride the bag as msgpack ``bin``, which reaches Python as ``bytes``, and
``dtype`` says how to read those bytes. So this cast composes nothing
surface-shaped — there is no surface id, no claim and no lifetime to hold, and
``ClaimedSurfacePixelAccess`` demands all three of a type that composes it.

What ``samples`` guarantees is that reading the block adds no copy of its
payload: the numpy array is a view over the bag's own ``bytes``. The copies
between shared memory and ``process()`` are the helper hop every bag pays and
this cast removes none of them.
"""

from __future__ import annotations

import typing
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Any, Literal

if typing.TYPE_CHECKING:
    from numpy.typing import NDArray

__all__ = ["AudioBlock"]

AudioSampleDtype = Literal["f32", "i16"]

# Keyed by the same wire vocabulary: how to read one scalar, and how wide it
# is. The numpy spelling is explicitly little-endian — never the platform's
# native one — because the payload is little-endian by contract and not by
# luck, and a big-endian reader taking the native spelling would decode
# every sample wrong rather than fail.
_NUMPY_TYPE_FOR_DTYPE: "dict[str, str]" = {"f32": "<f4", "i16": "<i2"}
_BYTES_PER_SAMPLE_FOR_DTYPE: "dict[str, int]" = {"f32": 4, "i16": 2}

# The keys without which there is no block to speak of. Everything else the
# bag carries is optional, this cast's or somebody else's.
_REQUIRED_BAG_KEYS = (
    "samples",
    "sample_rate",
    "channels",
    "sample_count",
    "first_sample_timestamp_ns",
)


def _is_plain_int(value: Any) -> bool:
    """`bool` is an `int` subclass; a channel count of `True` is a bug."""
    return isinstance(value, int) and not isinstance(value, bool)


@dataclass(frozen=True, init=False)
class AudioBlock:
    """An audio-block bag, cast: interleaved CPU samples plus how to read them.

    ``first_sample_timestamp_ns`` (the machine's monotonic clock in
    nanoseconds, comparable across every process on the host) is the ordering
    primitive and the whole of A/V sync: any sample's instant derives from it,
    ``sample_count`` and ``sample_rate``, so joining a block to a camera frame
    is subtracting two timestamps.

    Read through ``ctx.inputs.read(port, into=AudioBlock)``, whose bag keys
    this validates on construction; ``samples`` is then the numpy view of the
    payload.
    """

    # Off the repr: a block is thousands of scalars, and a failed assertion
    # that prints all of them buries what it was actually about.
    interleaved_sample_bytes: bytes = field(repr=False)
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
        """Validate the bag's entries and assign them.

        Written out rather than generated because the bag is an open map: a
        producer may carry keys this cast does not read, and the day one adds
        a key must not be the day every `read(port, into=AudioBlock)` starts
        raising. Construction is the validation, so that spelling and
        `from_bag` are one path rather than two that can drift.
        """
        del keys_this_cast_does_not_read
        if not isinstance(samples, bytes):
            raise ValueError(
                "bag is not an audio block: 'samples' must be bytes — the payload "
                "rides the bag as msgpack bin"
            )
        if (
            not _is_plain_int(sample_rate)
            or not _is_plain_int(channels)
            or not _is_plain_int(sample_count)
            or not _is_plain_int(first_sample_timestamp_ns)
        ):
            raise ValueError(
                "bag is not an audio block: sample_rate/channels/sample_count/"
                "first_sample_timestamp_ns must be int"
            )
        # Before the length check, which two negatives would slip through by
        # cancelling: a payload of four bytes satisfies
        # `sample_count=-1 × channels=-1 × 4`, and the block would then fail at
        # `reshape` rather than here. The Rust cast spells these `u32`, where
        # the case cannot arise at all.
        if sample_count < 0 or channels < 0:
            raise ValueError(
                f"bag is not an audio block: sample_count={sample_count} and "
                f"channels={channels} must both be non-negative"
            )
        if dtype not in _NUMPY_TYPE_FOR_DTYPE:
            raise ValueError(
                f"bag is not an audio block: dtype {dtype!r} is not one this cast "
                f"reads ({', '.join(sorted(_NUMPY_TYPE_FOR_DTYPE))})"
            )
        # Checked here rather than left to `reshape`, which would take a
        # payload of the wrong length and either raise about shapes or — when
        # the length happens to divide — hand back a plausible-looking wrong
        # answer.
        bytes_per_sample = _BYTES_PER_SAMPLE_FOR_DTYPE[dtype]
        expected_byte_count = sample_count * channels * bytes_per_sample
        if len(samples) != expected_byte_count:
            raise ValueError(
                f"bag is not an audio block: 'samples' carries {len(samples)} bytes, "
                f"but sample_count={sample_count} × channels={channels} × "
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
        """The payload as a ``(sample_count, channels)`` numpy array.

        A view over ``interleaved_sample_bytes``, so it costs no copy of the
        payload and is read-only — the bag's bytes are immutable. Take a copy
        (``numpy.array(block.samples)``) to write into it.
        """
        # Imported here so the wheel never takes a numpy dependency: a user
        # reaching for the array already has numpy.
        import numpy

        return numpy.frombuffer(
            self.interleaved_sample_bytes, dtype=_NUMPY_TYPE_FOR_DTYPE[self.dtype]
        ).reshape(self.sample_count, self.channels)

    @classmethod
    def from_bag(cls, bag: Mapping[str, Any]) -> "AudioBlock":
        """Construct from a bag dict, raising ValueError on a missing or
        mistyped key.

        The same *validation* `read(port, into=AudioBlock)` performs, so the
        two spellings cannot disagree about what a valid block is; this one
        only names the missing key, which keyword construction reports in
        Python's words.
        """
        missing = [key for key in _REQUIRED_BAG_KEYS if key not in bag]
        if missing:
            raise ValueError(f"bag is not an audio block: missing key {missing[0]!r}")
        return cls(**bag)
