# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.AudioBlock` — the audio record's cast, over a live link.

An audio block carries its samples inline, so what has to survive the wire is
the samples' msgpack *type*: `bin`, which unpacks as `bytes`. That is what
these tests drive, over real wired iceoryx2 ports rather than a stand-in,
because a message has to survive the wire before `into=` has anything to cast.

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

from tatolab.runtime import _engine
from tatolab.stream import AudioBlock, NodeLinkDataAccess, msgpack

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)

OUTPUT_PORT = "audio_to_downstream"
INPUT_PORT = "audio_from_upstream"


def interleaved_f32_bytes(scalars: "list[float]") -> bytes:
    return struct.pack(f"<{len(scalars)}f", *scalars)


def interleaved_i16_bytes(scalars: "list[int]") -> bytes:
    return struct.pack(f"<{len(scalars)}h", *scalars)


def stereo_block_record(scalars: "list[float]") -> "dict[str, Any]":
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
        self, source: NodeLinkDataAccess, destination: NodeLinkDataAccess
    ) -> None:
        self.source = source
        self.destination = destination

    def deliver(self, record: "dict[str, Any]") -> None:
        self.source.write_to_output_port(OUTPUT_PORT, msgpack.pack(record))


@pytest.fixture
def wired_link(request: pytest.FixtureRequest) -> Iterator[WiredLinkUnderTest]:
    """A source and a destination joined by one link.

    Service names carry the test's own name because every test in this
    process shares one iceoryx2 domain — a fixed name would let one test's
    channel meet the next one's.
    """
    unique = f"pinto{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/audio"
    notify_service_name = f"{unique}_dest/notify"
    link_id = f"L-{unique}"

    destination = _engine.open_node_link_data_access_for_helper_process()
    destination.wire_input_link(
        INPUT_PORT,
        channel_service_name,
        channel_service_name,
        notify_service_name,
        "read_next_in_order",
        8,
        8,
        2,
        1,
        link_id,
    )
    source = _engine.open_node_link_data_access_for_helper_process()
    source.wire_output_link(
        OUTPUT_PORT,
        channel_service_name,
        notify_service_name,
        1024,
        1 << 20,
        8,
        2,
        1,
        link_id,
    )
    yield WiredLinkUnderTest(source, destination)


def test_the_samples_cross_the_wire_as_bytes(wired_link: WiredLinkUnderTest):
    """The contract underneath the cast: `samples` is a byte buffer on the
    wire, so the unpacked record holds `bytes` rather than a list of numbers.

    A producer whose samples encoded as a msgpack array would still read back
    equal-looking data here — as a `list`, five bytes per sample instead of
    four, and unreadable as a buffer by a consumer in another language.
    """
    samples = interleaved_f32_bytes([-1.0, -0.5, 0.0, 0.5])
    wired_link.deliver(stereo_block_record([-1.0, -0.5, 0.0, 0.5]))

    payload = wired_link.destination.read_from_input_port(INPUT_PORT)

    assert payload is not None
    record = msgpack.unpack(payload)
    assert type(record["samples"]) is bytes
    assert record["samples"] == samples


def test_a_block_read_off_the_wire_casts_to_an_audio_block(
    wired_link: WiredLinkUnderTest,
):
    wired_link.deliver(stereo_block_record([-1.0, -0.5, 0.0, 0.5]))

    block = wired_link.destination.read_from_input_port(INPUT_PORT, into=AudioBlock)

    assert isinstance(block, AudioBlock)
    assert block.sample_rate == 48_000
    assert block.channels == 2
    assert block.sample_count == 2
    assert block.dtype == "f32"
    assert block.first_sample_timestamp_ns == 123_456_789


def test_the_samples_are_a_numpy_view_over_the_unpacked_bytes(
    wired_link: WiredLinkUnderTest,
):
    """The one thing the cast guarantees about copies: it adds none.

    The copies between shared memory and here are the helper hop every
    message pays and this changes nothing about them — but the array is a view
    over the bytes the unpack produced, not another copy of them. Asserted by
    identity through numpy's base chain, which `shares_memory` alone would not
    show: two copies of the same values do not share memory, but neither does
    that prove which object the array is looking at.
    """
    wired_link.deliver(stereo_block_record([-1.0, -0.5, 0.0, 0.5]))

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
    assert not samples.flags.writeable, "the unpacked bytes are immutable"


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
    truncated = stereo_block_record([-1.0, -0.5, 0.0, 0.5])
    truncated["sample_count"] = 3
    wired_link.deliver(truncated)

    with pytest.raises(ValueError, match="sample_count=3 × channels=2"):
        wired_link.destination.read_from_input_port(INPUT_PORT, into=AudioBlock)


def test_a_key_this_cast_does_not_read_does_not_break_the_read(
    wired_link: WiredLinkUnderTest,
):
    """The record is an open map: the day a producer adds a key must not be the
    day every `read(port, into=AudioBlock)` starts raising."""
    record = stereo_block_record([-1.0, -0.5, 0.0, 0.5])
    record["a_future_key"] = "ignored"
    wired_link.deliver(record)

    block = wired_link.destination.read_from_input_port(INPUT_PORT, into=AudioBlock)

    assert isinstance(block, AudioBlock)
    assert block.sample_count == 2


def test_a_window_the_engines_stage_emits_casts_to_an_audio_block(
    request: pytest.FixtureRequest,
):
    """The joint between the two languages: blocks packed here are decoded by
    the engine's audio-window stage, and the window it re-encodes in Rust is
    what `into=AudioBlock` builds from. A key renamed on either side surfaces
    here rather than at a microphone."""
    unique = f"window{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/audio"
    notify_service_name = f"{unique}_dest/notify"
    link_id = f"L-{unique}"
    destination = _engine.open_node_link_data_access_for_helper_process()
    destination.wire_input_link(
        INPUT_PORT, channel_service_name, channel_service_name,
        notify_service_name,
        "read_next_in_order", 8, 8, 2, 1, link_id,
        audio_window={
            "sample_rate": 16_000,
            "channels": 1,
            "dtype": "f32",
            "window_size": 512,
            "hop": 512,
        },
    )  # fmt: skip
    source = _engine.open_node_link_data_access_for_helper_process()
    source.wire_output_link(
        OUTPUT_PORT, channel_service_name, notify_service_name,
        16_384, 1 << 20, 8, 2, 1, link_id,
    )  # fmt: skip

    source_frames_per_block = 512
    for block_index in range(8):
        source.write_to_output_port(
            OUTPUT_PORT,
            AudioBlock(
                samples=interleaved_f32_bytes(
                    [0.25] * (source_frames_per_block * 2)
                ),
                sample_rate=48_000,
                channels=2,
                sample_count=source_frames_per_block,
                dtype="f32",
                first_sample_timestamp_ns=block_index
                * source_frames_per_block
                * 1_000_000_000
                // 48_000,
            ).to_bytes(),
        )

    window = destination.read_from_input_port(INPUT_PORT, into=AudioBlock)

    assert isinstance(window, AudioBlock)
    assert window.sample_rate == 16_000
    assert window.channels == 1
    assert window.sample_count == 512
    assert window.dtype == "f32"
    assert window.samples.shape == (512, 1)
