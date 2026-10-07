# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.AudioBlock` — the audio bag's cast, built from a bag in hand.

What the cast refuses and how it reads its fields, decided before any wire is
involved. That the payload survives a real link as `bytes` is tested against
the runtime.
"""

import struct
from typing import Any

import pytest

from tatolab.stream import AudioBlock
from tatolab.stream.audio_block import _NUMPY_TYPE_FOR_DTYPE


def interleaved_f32_bytes(scalars: "list[float]") -> bytes:
    return struct.pack(f"<{len(scalars)}f", *scalars)


def stereo_block_bag(scalars: "list[float]") -> "dict[str, Any]":
    """A two-channel `f32` block carrying `scalars`, interleaved."""
    return {
        "samples": interleaved_f32_bytes(scalars),
        "sample_rate": 48_000,
        "channels": 2,
        "sample_count": len(scalars) // 2,
        "dtype": "f32",
        "first_sample_timestamp_ns": 123_456_789,
    }


def test_a_block_with_no_dtype_reads_as_f32():
    """`dtype` is metadata with a default, so a producer that omits it is
    describing an `f32` block rather than an unreadable one."""
    bag = stereo_block_bag([1.0, 2.0])
    del bag["dtype"]

    block = AudioBlock.from_bag(bag)

    assert block.dtype == "f32"
    assert block.samples.tolist() == [[1.0, 2.0]]


def test_a_dtype_this_cast_cannot_read_is_refused_by_name():
    bag = stereo_block_bag([1.0, 2.0])
    bag["dtype"] = "f64"

    with pytest.raises(ValueError, match="dtype 'f64' is not one this cast reads"):
        AudioBlock.from_bag(bag)


def test_a_payload_that_is_not_bytes_is_refused_by_name():
    """The mistake this catches is the one that otherwise decodes silently: a
    producer whose samples went out as a list of numbers rather than a
    buffer."""
    bag = stereo_block_bag([1.0, 2.0])
    bag["samples"] = [1.0, 2.0]

    with pytest.raises(ValueError, match="'samples' must be bytes"):
        AudioBlock.from_bag(bag)


def test_a_missing_key_is_named():
    bag = stereo_block_bag([1.0, 2.0])
    del bag["sample_rate"]

    with pytest.raises(ValueError, match="missing key 'sample_rate'"):
        AudioBlock.from_bag(bag)


def test_an_audio_block_takes_no_surface_and_holds_no_claim():
    """Audio touches no surface machinery at all — the cast composes nothing
    that would demand a surface id or take a claim, which is why a block is
    constructible from a bag this test wrote by hand."""
    block = AudioBlock.from_bag(stereo_block_bag([1.0, 2.0]))

    assert not hasattr(block, "surface_id")
    assert not hasattr(block, "writable")
    assert not hasattr(block, "__dlpack__")


def test_the_numpy_types_are_spelled_little_endian_at_the_source():
    """The one decision on this cast no behavioural assertion can catch.

    numpy answers the native spelling and the little-endian spelling with the
    same dtype on a little-endian host, and the platform floor is little-endian
    — so a cast that asked for `"f4"` would pass every other test in this file
    while decoding every sample wrong for a big-endian reader. What protects
    that reader is the spelling itself, so the spelling is what this asserts.
    """
    assert _NUMPY_TYPE_FOR_DTYPE == {"f32": "<f4", "i16": "<i2"}


def test_negative_dimensions_are_refused_rather_than_cancelling():
    """Two negatives multiply back to a length the payload satisfies, so the
    length check alone would pass them through to `reshape`."""
    bag = stereo_block_bag([1.0])
    bag["sample_count"] = -1
    bag["channels"] = -1

    with pytest.raises(ValueError, match="must both be non-negative"):
        AudioBlock.from_bag(bag)
