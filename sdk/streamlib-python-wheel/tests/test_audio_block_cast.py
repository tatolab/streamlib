# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`streamlib.AudioBlock` — the audio bag's cast, over a live link.

An audio block carries its payload inline, so what has to survive the wire is
the payload's msgpack *type*: `bin`, which reaches Python as `bytes`. That is
what these tests drive, over real wired iceoryx2 ports rather than a stand-in,
because a bag has to survive the wire before `into=` has anything to cast.

Both ends live on this thread because iceoryx2's ports are `!Send`, and the
destination is wired first because a send with no subscriber attached is
dropped.
"""

import os
import struct
from collections.abc import Iterator
from typing import Any

import numpy
import pytest

from streamlib import AudioBlock, ProcessorLinkDataAccess
from streamlib.audio_block import _NUMPY_TYPE_FOR_DTYPE

OUTPUT_PORT = "audio_to_downstream"
INPUT_PORT = "audio_from_upstream"


def interleaved_f32_bytes(scalars: "list[float]") -> bytes:
    return struct.pack(f"<{len(scalars)}f", *scalars)


def interleaved_i16_bytes(scalars: "list[int]") -> bytes:
    return struct.pack(f"<{len(scalars)}h", *scalars)


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


class WiredLinkUnderTest:
    """One live link, from the writing end to the reading end."""

    def __init__(
        self, source: ProcessorLinkDataAccess, destination: ProcessorLinkDataAccess
    ) -> None:
        self.source = source
        self.destination = destination

    def deliver(self, bag: "dict[str, Any]") -> None:
        self.source.write_to_output_port(OUTPUT_PORT, bag)


@pytest.fixture
def wired_link(request: pytest.FixtureRequest) -> Iterator[WiredLinkUnderTest]:
    """A source and a destination joined by one link.

    Service names carry the pid and the test's own name because iceoryx2
    service state is machine-global and outlives a crashed process — a fixed
    name would let one bad run poison every later one.
    """
    unique = f"pinto{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/audio"
    notify_service_name = f"{unique}_dest/notify"
    link_id = f"L-{unique}"

    destination = ProcessorLinkDataAccess()
    destination.wire_input_link(
        INPUT_PORT,
        channel_service_name,
        notify_service_name,
        "read_next_in_order",
        8,
        2,
        1,
        True,
        link_id,
    )
    source = ProcessorLinkDataAccess()
    source.wire_output_link(
        OUTPUT_PORT,
        channel_service_name,
        notify_service_name,
        1024,
        1 << 20,
        8,
        2,
        1,
        True,
        link_id,
    )
    yield WiredLinkUnderTest(source, destination)


def test_the_payload_crosses_the_wire_as_bytes(wired_link: WiredLinkUnderTest):
    """The contract underneath the cast: `samples` is a byte buffer on the
    wire, so an untyped read hands back `bytes` rather than a list of numbers.

    A producer whose samples encoded as a msgpack array would still read back
    equal-looking data here — as a `list`, five bytes per sample instead of
    four, and unreadable as a buffer by a consumer in another language.
    """
    payload = interleaved_f32_bytes([-1.0, -0.5, 0.0, 0.5])
    wired_link.deliver(stereo_block_bag([-1.0, -0.5, 0.0, 0.5]))

    bag = wired_link.destination.read_from_input_port(INPUT_PORT)

    assert bag is not None
    assert type(bag["samples"]) is bytes
    assert bag["samples"] == payload


def test_a_block_read_off_the_wire_casts_to_an_audio_block(
    wired_link: WiredLinkUnderTest,
):
    wired_link.deliver(stereo_block_bag([-1.0, -0.5, 0.0, 0.5]))

    block = wired_link.destination.read_from_input_port(INPUT_PORT, into=AudioBlock)

    assert isinstance(block, AudioBlock)
    assert block.sample_rate == 48_000
    assert block.channels == 2
    assert block.sample_count == 2
    assert block.dtype == "f32"
    assert block.first_sample_timestamp_ns == 123_456_789


def test_the_samples_are_a_numpy_view_over_the_bag_bytes(
    wired_link: WiredLinkUnderTest,
):
    """The one thing the cast guarantees about copies: it adds none.

    The four copies between shared memory and here are the helper hop every
    bag pays and this changes nothing about them — but the array is a view
    over the bytes the read produced, not a fifth copy of them. Asserted by
    identity through numpy's base chain, which `shares_memory` alone would not
    show: two copies of the same values do not share memory, but neither does
    that prove which object the array is looking at.
    """
    wired_link.deliver(stereo_block_bag([-1.0, -0.5, 0.0, 0.5]))

    block = wired_link.destination.read_from_input_port(INPUT_PORT, into=AudioBlock)
    assert isinstance(block, AudioBlock)
    samples = block.samples

    viewed = samples
    while viewed.base is not None and isinstance(viewed.base, numpy.ndarray):
        viewed = viewed.base
    assert viewed.base is block.interleaved_sample_bytes
    assert numpy.shares_memory(samples, numpy.frombuffer(block.interleaved_sample_bytes, "<f4"))
    assert samples.tolist() == [[-1.0, -0.5], [0.0, 0.5]]
    assert samples.shape == (2, 2)
    assert samples.dtype == numpy.dtype("<f4")
    assert not samples.flags.writeable, "the bag's bytes are immutable"


def test_an_i16_block_reads_through_the_same_field(wired_link: WiredLinkUnderTest):
    """One field spelling serves both dtypes: `dtype` is what decides how the
    bytes are read, and the array's type is the little-endian `i16`."""
    wired_link.deliver(
        {
            "samples": interleaved_i16_bytes([-32768, -1, 0, 32767]),
            "sample_rate": 16_000,
            "channels": 1,
            "sample_count": 4,
            "dtype": "i16",
            "first_sample_timestamp_ns": 7,
        }
    )

    block = wired_link.destination.read_from_input_port(INPUT_PORT, into=AudioBlock)

    assert isinstance(block, AudioBlock)
    assert block.samples.dtype == numpy.dtype("<i2")
    assert block.samples.tolist() == [[-32768], [-1], [0], [32767]]


def test_a_payload_of_the_wrong_length_is_refused_at_the_read(
    wired_link: WiredLinkUnderTest,
):
    """A block whose payload does not hold `sample_count × channels` scalars
    is a producer bug, and the read is where it has to surface — reshaped, it
    would be a plausible-looking wrong answer or an error about shapes that
    names nothing."""
    truncated = stereo_block_bag([-1.0, -0.5, 0.0, 0.5])
    truncated["sample_count"] = 3
    wired_link.deliver(truncated)

    with pytest.raises(ValueError, match="sample_count=3 × channels=2"):
        wired_link.destination.read_from_input_port(INPUT_PORT, into=AudioBlock)


def test_a_key_this_cast_does_not_read_does_not_break_the_read(
    wired_link: WiredLinkUnderTest,
):
    """The bag map is open: the day a producer adds a key must not be the day
    every `read(port, into=AudioBlock)` starts raising."""
    bag = stereo_block_bag([-1.0, -0.5, 0.0, 0.5])
    bag["a_future_key"] = "ignored"
    wired_link.deliver(bag)

    block = wired_link.destination.read_from_input_port(INPUT_PORT, into=AudioBlock)

    assert isinstance(block, AudioBlock)
    assert block.sample_count == 2


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
