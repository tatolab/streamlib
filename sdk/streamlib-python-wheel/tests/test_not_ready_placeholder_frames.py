# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The transparent frames a not-ready node's Image outputs are sent."""

from typing import Any

import numpy

from tatolab.stream import Output

from tatolab.runtime._not_ready_placeholder_frames import (
    TransparentFramesOnTheImageOutputsOfANodeNotReady,
    the_placeholder_texture_format_of_an_image_output,
)


class TextureSlotFilledWithNonZeroBytes:
    def __init__(self, width: int, height: int, texture_format: str) -> None:
        self.surface_id = f"slot-{width}x{height}"
        self.width = width
        self.height = height
        self.format = texture_format
        self.pixels = numpy.full((height, width, 4), 0xAB, dtype=numpy.uint8)
        self.locked_for_writing = False

    def lock(self, read_only: bool = True) -> None:
        self.locked_for_writing = not read_only

    def unlock(self) -> None:
        self.locked_for_writing = False

    def as_numpy(self) -> Any:
        assert self.locked_for_writing, "a placeholder frame is written under a write lock"
        return self.pixels


class GpuContextHandingOutSlots:
    def __init__(self) -> None:
        self.slots_handed_out: "list[TextureSlotFilledWithNonZeroBytes]" = []

    def acquire_texture_from_node_output_pool(
        self, pool_key, depth, width, height, texture_format, texture_usage
    ):
        slot = TextureSlotFilledWithNonZeroBytes(width, height, texture_format)
        self.slots_handed_out.append(slot)
        return slot


class GpuContextRefusingEverySlot:
    def acquire_texture_from_node_output_pool(
        self, pool_key, depth, width, height, texture_format, texture_usage
    ):
        raise RuntimeError("no GPU in this helper")


class OutputsRecordingEachWrite:
    def __init__(self) -> None:
        self.writes: "list[tuple[str, Any]]" = []

    def write(self, port: Any, frame: Any) -> None:
        assert isinstance(port, Output), (
            "an Image frame is written through an Output attribute; by name a write takes only a buffer"
        )
        self.writes.append((port.port_name, frame))


class NodeContextOfTheTest:
    def __init__(self) -> None:
        self.gpu_limited_access: Any = GpuContextHandingOutSlots()
        self.outputs = OutputsRecordingEachWrite()


def test_each_image_output_is_sent_one_frame_every_channel_zero_at_the_named_size():
    warnings: "list[tuple[str, dict]]" = []
    frames = TransparentFramesOnTheImageOutputsOfANodeNotReady(
        [
            {"name": "video", "family": {"family": "image", "formats": ["rgba8_unorm"]}},
            {"name": "matte", "family": {"family": "image"}},
            {"name": "levels", "family": {"family": "records"}},
        ],
        lambda message, fields: warnings.append((message, fields)),
    )
    context = NodeContextOfTheTest()

    frames.send_one_transparent_frame_on_each_image_output(context, 64, 32)

    assert [port for port, _ in context.outputs.writes] == ["video", "matte"]
    for slot in context.gpu_limited_access.slots_handed_out:
        assert (slot.width, slot.height, slot.format) == (64, 32, "rgba8_unorm")
        assert not slot.pixels.any(), "every channel of a placeholder frame is zero"
    for _, frame in context.outputs.writes:
        assert (frame.width, frame.height) == (64, 32)
    assert warnings == []


def test_a_frame_that_cannot_be_made_is_dropped_and_warned_of():
    warnings: "list[tuple[str, dict]]" = []
    frames = TransparentFramesOnTheImageOutputsOfANodeNotReady(
        [{"name": "video", "family": {"family": "image"}}],
        lambda message, fields: warnings.append((message, fields)),
    )
    context = NodeContextOfTheTest()
    context.gpu_limited_access = GpuContextRefusingEverySlot()
    frames.send_one_transparent_frame_on_each_image_output(context, 64, 32)

    assert context.outputs.writes == []
    assert warnings and warnings[0][1]["error"] == "RuntimeError: no GPU in this helper"


def test_an_image_output_declaring_only_formats_without_alpha_is_sent_nothing():
    assert the_placeholder_texture_format_of_an_image_output([]) == "rgba8_unorm"
    assert the_placeholder_texture_format_of_an_image_output(["nv12", "bgra8_unorm"]) == "bgra8_unorm"
    assert the_placeholder_texture_format_of_an_image_output(["nv12"]) is None
