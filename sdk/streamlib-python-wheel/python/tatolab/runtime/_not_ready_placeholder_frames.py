# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The transparent frames the runtime sends on a not-ready node's Image outputs.

A node that reports itself not ready — a file setting left unset, a file that
is missing — keeps its Image outputs fed so the stream downstream of it keeps
running: each frame is every channel zero, transparent at the size the node
named, never a colour that could reach a stage screen. Array, Channels and
Records outputs send nothing meanwhile.
"""

from __future__ import annotations

from typing import Any, Callable, Optional

from tatolab.stream import ColorInfo, Image, ImageFrameToWrite, NodeOutputTextureRing, Output

#: The texture formats in which every channel zero is a transparent pixel.
TEXTURE_FORMATS_TRANSPARENT_WHEN_ZEROED = (
    "rgba8_unorm",
    "rgba8_unorm_srgb",
    "bgra8_unorm",
    "bgra8_unorm_srgb",
    "rgba16_float",
    "rgba32_float",
)

#: What an Image output that names no format is sent.
PLACEHOLDER_TEXTURE_FORMAT_OF_AN_OUTPUT_NAMING_NONE = "rgba8_unorm"

#: Two slots, so a frame sent while a reader holds the one before it never waits.
PLACEHOLDER_OUTPUT_RING_DEPTH = 2

#: How often a not-ready node the runtime does not call on a schedule — one
#: declared `execution="manual"` — has its Image outputs sent a frame.
PLACEHOLDER_FRAMES_PER_SECOND_OF_A_MANUAL_NODE = 30

PLACEHOLDER_FRAME_COLOUR = ColorInfo(
    primaries="bt709", transfer="srgb", matrix="identity", range="full"
)


def the_placeholder_texture_format_of_an_image_output(
    declared_formats: "list[str]",
) -> Optional[str]:
    """The format a not-ready node's Image output is sent, or `None` when every
    format it declares has no transparent pixel."""
    if not declared_formats:
        return PLACEHOLDER_TEXTURE_FORMAT_OF_AN_OUTPUT_NAMING_NONE
    for declared_format in declared_formats:
        if declared_format in TEXTURE_FORMATS_TRANSPARENT_WHEN_ZEROED:
            return declared_format
    return None


def image_output_attribute_naming_the_port(port_name: str) -> "Output[Any]":
    """An `Output(Image)` attribute naming `port_name`, the way `ctx.outputs.write`
    takes an Image frame; written by name it takes only a buffer."""
    image_output_attribute: "Output[Any]" = Output(Image)
    image_output_attribute.__set_name__(TransparentFramesOnTheImageOutputsOfANodeNotReady, port_name)
    return image_output_attribute


class TransparentFramesOnTheImageOutputsOfANodeNotReady:
    """Sends one transparent frame on each Image output of a node, per call."""

    def __init__(
        self,
        declared_output_ports: "list[dict[str, Any]]",
        warn_once: Callable[[str, "dict[str, Any]"], None],
    ) -> None:
        self._texture_ring_by_image_output: "dict[str, NodeOutputTextureRing]" = {}
        self._output_attribute_by_image_output: "dict[str, Output[Any]]" = {}
        for port in declared_output_ports:
            family_declaration = port.get("family") or {}
            if family_declaration.get("family") != "image":
                continue
            texture_format = the_placeholder_texture_format_of_an_image_output(
                list(family_declaration.get("formats") or [])
            )
            if texture_format is None:
                warn_once(
                    "this not-ready node's Image output declares no format with a transparent "
                    "pixel, so it is sent nothing while the node is not ready",
                    {"port": port["name"], "formats": family_declaration.get("formats")},
                )
                continue
            self._texture_ring_by_image_output[port["name"]] = NodeOutputTextureRing(
                texture_format, ["texture_binding"], depth=PLACEHOLDER_OUTPUT_RING_DEPTH
            )
            self._output_attribute_by_image_output[port["name"]] = (
                image_output_attribute_naming_the_port(port["name"])
            )
        self._warn_once = warn_once

    def send_one_transparent_frame_on_each_image_output(
        self, context: Any, image_width: int, image_height: int
    ) -> None:
        """Write one transparent `image_width` by `image_height` frame on each Image output.

        A frame that cannot be made or written is dropped and warned of once;
        the next call tries again.
        """
        for port_name, texture_ring in self._texture_ring_by_image_output.items():
            try:
                slot = texture_ring.next_texture_for_this_frame(
                    context.gpu_limited_access, image_width, image_height
                )
                slot.lock(read_only=False)
                try:
                    slot.as_numpy()[...] = 0
                finally:
                    slot.unlock()
                context.outputs.write(
                    self._output_attribute_by_image_output[port_name],
                    ImageFrameToWrite.of_texture(slot, PLACEHOLDER_FRAME_COLOUR),
                )
            except Exception as refusal:
                self._warn_once(
                    "a transparent frame for this not-ready node's Image output was dropped",
                    {"port": port_name, "error": f"{type(refusal).__name__}: {refusal}"},
                )
