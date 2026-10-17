# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.AudioBlock` — the audio-block message's cast, read from a payload in hand.

What the cast refuses and how it reads its fields, decided before any wire is
involved.
"""

import struct
from typing import Any

import pytest

from tatolab.stream import AudioBlock
from tatolab.stream.audio_block import _NUMPY_TYPE_FOR_DTYPE
from tatolab.stream.msgpack import pack, unpack


def interleaved_f32_bytes(scalars: "list[float]") -> bytes:
    return struct.pack(f"<{len(scalars)}f", *scalars)


def stereo_block_entries(scalars: "list[float]") -> "dict[str, Any]":
    """A two-channel `f32` block carrying `scalars`, interleaved."""
    return {
        "samples": interleaved_f32_bytes(scalars),
        "sample_rate": 48_000,
        "channels": 2,
        "sample_count": len(scalars) // 2,
        "dtype": "f32",
        "first_sample_timestamp_ns": 123_456_789,
    }


def test_from_bytes_reads_the_samples_as_a_numpy_view():
    block = AudioBlock.from_bytes(pack(stereo_block_entries([1.0, 2.0, 3.0, 4.0])))

    assert block.samples.tolist() == [[1.0, 2.0], [3.0, 4.0]]
    assert (block.sample_rate, block.channels, block.sample_count) == (48_000, 2, 2)
    assert block.first_sample_timestamp_ns == 123_456_789


def test_to_bytes_writes_the_samples_under_the_wire_key_and_round_trips():
    entries = stereo_block_entries([1.0, 2.0])
    block = AudioBlock.from_bytes(pack(entries))

    assert unpack(block.to_bytes()) == entries
    assert AudioBlock.from_bytes(block.to_bytes()) == block


def test_a_block_with_no_dtype_reads_as_f32():
    entries = stereo_block_entries([1.0, 2.0])
    del entries["dtype"]

    block = AudioBlock.from_bytes(pack(entries))

    assert block.dtype == "f32"
    assert block.samples.tolist() == [[1.0, 2.0]]


def test_a_dtype_this_cast_cannot_read_is_refused_by_name():
    entries = stereo_block_entries([1.0, 2.0])
    entries["dtype"] = "f64"

    with pytest.raises(ValueError, match="dtype 'f64' is not one this cast reads"):
        AudioBlock.from_bytes(pack(entries))


def test_samples_that_are_not_bytes_are_refused_by_name():
    """A producer whose samples went out as a list of numbers rather than a
    buffer would otherwise decode silently."""
    entries = stereo_block_entries([1.0, 2.0])
    entries["samples"] = [1.0, 2.0]

    with pytest.raises(ValueError, match="'samples' must be bytes"):
        AudioBlock.from_bytes(pack(entries))


def test_a_missing_key_is_named():
    entries = stereo_block_entries([1.0, 2.0])
    del entries["sample_rate"]

    with pytest.raises(TypeError, match="sample_rate"):
        AudioBlock.from_bytes(pack(entries))


def test_an_audio_block_takes_no_surface_and_holds_no_claim():
    block = AudioBlock.from_bytes(pack(stereo_block_entries([1.0, 2.0])))

    assert not hasattr(block, "surface_id")
    assert not hasattr(block, "writable")
    assert not hasattr(block, "__dlpack__")


def test_the_numpy_types_are_spelled_little_endian_at_the_source():
    """numpy answers the native and the little-endian spelling with the same
    dtype on a little-endian host, so only the spelling protects a big-endian
    reader."""
    assert _NUMPY_TYPE_FOR_DTYPE == {"f32": "<f4", "i16": "<i2"}


def test_negative_dimensions_are_refused_rather_than_cancelling():
    entries = stereo_block_entries([1.0])
    entries["sample_count"] = -1
    entries["channels"] = -1

    with pytest.raises(ValueError, match="must both be non-negative"):
        AudioBlock.from_bytes(pack(entries))
