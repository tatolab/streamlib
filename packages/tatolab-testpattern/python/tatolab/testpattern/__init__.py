# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`TestPatternSource`: SMPTE bars, a bouncing potato and the sample stream's captions, on the GPU.

A frame's picture is a function of its index alone: the potato leaves the
top-left corner at frame zero and moves four pixels a frame along each axis,
bouncing off every edge, and the caption panel sits in the bottom-right corner.
One compute dispatch per frame writes the picture into a texture from the
node's output pool.
"""

from __future__ import annotations

import ctypes
import dataclasses
import struct
from importlib import resources
from typing import Annotated, Callable

from tatolab.stream import (
    ColorInfo,
    Description,
    GpuContextFullAccess,
    GpuSurfaceHandle,
    Input,
    TextureToWrite,
    NodeOutputTextureRing,
    NumberRange,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    Texture,
    TexturePixelFormats,
    computed,
    log,
    monotonic_now_ns,
    node,
)

__all__ = ["TestPatternSource"]

_THIS_PACKAGE = "tatolab.testpattern"

TEST_PATTERN_FRAME_COLOUR = ColorInfo(primaries="bt709", transfer="srgb", range="full")

LARGEST_TEST_PATTERN_FRAME_SIDE_PIXELS = 16_384
"""The largest frame side the source renders: 16K, far past any frame a stream asks for."""

POTATO_SPRITE_PIXELS_PER_FRAME = 4
"""How far the potato travels along each axis per frame."""

CAPTION_PANEL_MARGIN_PIXELS = 24
"""The gap between the caption panel and the frame's right and bottom edges."""

DROPPED_FRAMES_BETWEEN_REPORTS = 1800
"""A run of frames the output pool refused is logged at its first and then at every this many."""

_TEST_PATTERN_FRAME_TEXTURE_FORMAT = "rgba8_unorm"
_TEST_PATTERN_FRAME_TEXTURE_USAGE = ["storage_binding", "texture_binding"]
_OVERLAY_TEXTURE_USAGE = ["texture_binding"]

# The shader's `local_size_x` and `local_size_y`.
_WORKGROUP_TILE_SIZE_IN_PIXELS = 16

_POTATO_SPRITE_BINDING_NAME = "potato_sprite_texture"
_CAPTION_PANEL_BINDING_NAME = "caption_panel_texture"
_TEST_PATTERN_FRAME_BINDING_NAME = "test_pattern_frame"

# The shader's `TestPatternFramePushConstants` block, field for field.
_PUSH_CONSTANT_BLOCK_STRUCT = struct.Struct("<IIiiIIiiII")

_SIGNED_PIXEL_OFFSET_RANGE = (-(2**31), 2**31 - 1)


@dataclasses.dataclass(frozen=True)
class BakedRgbaOverlay:
    """A picture shipped in this package as tightly packed RGBA rows."""

    resource_name: str
    width: int
    height: int

    def rgba_rows(self) -> bytes:
        """The overlay's `width × height` RGBA pixels, row by row."""
        rgba = resources.files(_THIS_PACKAGE).joinpath(self.resource_name).read_bytes()
        expected_byte_count = self.width * self.height * 4
        if len(rgba) != expected_byte_count:
            raise RuntimeError(
                f"TestPatternSource: {self.resource_name} holds {len(rgba)} bytes, not the "
                f"{expected_byte_count} of {self.width}x{self.height} RGBA pixels"
            )
        return rgba


POTATO_SPRITE = BakedRgbaOverlay("potato_sprite_160x160.rgba", 160, 160)
"""The potato, drawn over the bars wherever its frame index puts it."""

SAMPLE_STREAM_CAPTION_PANEL = BakedRgbaOverlay("sample_stream_captions_548x62.rgba", 548, 62)
""""Tatolab stream setup successfully" over "sample stream", right-aligned, in Press Start 2P
white on an opaque dark panel."""


@dataclasses.dataclass(frozen=True)
class OverlayTopLeftInTheFrame:
    """Where an overlay's top-left pixel lands; negative where the frame is smaller than it."""

    left: int
    top: int


def _saturated_to_a_signed_pixel_offset(pixel_offset: int) -> int:
    smallest, largest = _SIGNED_PIXEL_OFFSET_RANGE
    return max(smallest, min(largest, pixel_offset))


def _bounced_position_along(distance: int, travel: int) -> int:
    if travel == 0:
        return 0
    distance_into_the_round_trip = distance % (2 * travel)
    if distance_into_the_round_trip <= travel:
        return distance_into_the_round_trip
    return 2 * travel - distance_into_the_round_trip


def potato_sprite_top_left_at_frame(
    frame_index: int, frame_width: int, frame_height: int
) -> OverlayTopLeftInTheFrame:
    """Where the potato sits in frame `frame_index`, bouncing off every edge it reaches."""
    distance = (frame_index * POTATO_SPRITE_PIXELS_PER_FRAME) % 2**64
    return OverlayTopLeftInTheFrame(
        left=_bounced_position_along(distance, max(frame_width - POTATO_SPRITE.width, 0)),
        top=_bounced_position_along(distance, max(frame_height - POTATO_SPRITE.height, 0)),
    )


def caption_panel_top_left(frame_width: int, frame_height: int) -> OverlayTopLeftInTheFrame:
    """Where the caption panel sits: the bottom-right corner, inset by its margin."""

    def inset_from_the_far_edge(frame_side: int, panel_side: int) -> int:
        return _saturated_to_a_signed_pixel_offset(
            frame_side - panel_side - CAPTION_PANEL_MARGIN_PIXELS
        )

    return OverlayTopLeftInTheFrame(
        left=inset_from_the_far_edge(frame_width, SAMPLE_STREAM_CAPTION_PANEL.width),
        top=inset_from_the_far_edge(frame_height, SAMPLE_STREAM_CAPTION_PANEL.height),
    )


def pack_push_constants_for_frame(frame_index: int, frame_width: int, frame_height: int) -> bytes:
    """The kernel's push-constant block for frame `frame_index`."""
    potato_sprite = potato_sprite_top_left_at_frame(frame_index, frame_width, frame_height)
    caption_panel = caption_panel_top_left(frame_width, frame_height)
    return _PUSH_CONSTANT_BLOCK_STRUCT.pack(
        frame_width,
        frame_height,
        potato_sprite.left,
        potato_sprite.top,
        POTATO_SPRITE.width,
        POTATO_SPRITE.height,
        caption_panel.left,
        caption_panel.top,
        SAMPLE_STREAM_CAPTION_PANEL.width,
        SAMPLE_STREAM_CAPTION_PANEL.height,
    )


def compute_kernel_slang_rendering_a_test_pattern_frame() -> str:
    """The compute kernel that renders one frame."""
    return resources.files(_THIS_PACKAGE).joinpath("test_pattern_frame.slang").read_text()


def _workgroups_covering(pixels: int) -> int:
    return (pixels + _WORKGROUP_TILE_SIZE_IN_PIXELS - 1) // _WORKGROUP_TILE_SIZE_IN_PIXELS


def _upload_overlay_into_a_texture(
    gpu_full_access: GpuContextFullAccess, overlay: BakedRgbaOverlay
) -> GpuSurfaceHandle:
    rgba_rows = overlay.rgba_rows()
    overlay_texture = gpu_full_access.acquire_texture(
        overlay.width, overlay.height, _TEST_PATTERN_FRAME_TEXTURE_FORMAT, _OVERLAY_TEXTURE_USAGE
    )
    row_byte_count = overlay.width * 4
    overlay_texture.lock(read_only=False)
    try:
        base_address = overlay_texture.base_address
        if base_address is None:
            raise RuntimeError(
                f"TestPatternSource: the {overlay.resource_name} texture opened no host mapping"
            )
        bytes_per_row = overlay_texture.bytes_per_row
        for row_index in range(overlay.height):
            ctypes.memmove(
                base_address + row_index * bytes_per_row,
                rgba_rows[row_index * row_byte_count : (row_index + 1) * row_byte_count],
                row_byte_count,
            )
    finally:
        overlay_texture.unlock()
    return overlay_texture


@node.meta(
    description=(
        "Synthetic SMPTE-style color bars with a bouncing potato and the sample stream's "
        "captions, rendered on the GPU — demos a pipeline with no camera attached"
    ),
)
@node(execution="continuous", interval_ms=33)
def TestPatternSource(
    *,
    width: Input[
        Annotated[
            int,
            NumberRange(minimum=1, maximum=LARGEST_TEST_PATTERN_FRAME_SIDE_PIXELS),
            Description("Frame width in pixels."),
        ]
    ] = Input(1280),
    height: Input[
        Annotated[
            int,
            NumberRange(minimum=1, maximum=LARGEST_TEST_PATTERN_FRAME_SIDE_PIXELS),
            Description("Frame height in pixels."),
        ]
    ] = Input(720),
    video: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(_TEST_PATTERN_FRAME_TEXTURE_FORMAT),
            Description("Test-pattern video frames"),
        ]
    ],
    ctx: RuntimeContextFullAccess,
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """The sample stream's animation at about 30 frames a second, as RGBA pictures.

    The frame size is read each frame, so a change takes effect from the next frame on.
    """

    frame_ring = NodeOutputTextureRing(
        _TEST_PATTERN_FRAME_TEXTURE_FORMAT, _TEST_PATTERN_FRAME_TEXTURE_USAGE
    )
    compute_kernel = ctx.gpu_limited_access.create_compute_kernel(
        source=compute_kernel_slang_rendering_a_test_pattern_frame(),
        push_constant_size=_PUSH_CONSTANT_BLOCK_STRUCT.size,
        bindings={
            _POTATO_SPRITE_BINDING_NAME: "sampled_texture",
            _CAPTION_PANEL_BINDING_NAME: "sampled_texture",
            _TEST_PATTERN_FRAME_BINDING_NAME: "storage_image",
        },
    )
    # Each overlay is closed with the node's scope, releasing its GPU surface when the node stops.
    potato_sprite_texture = computed(
        lambda: _upload_overlay_into_a_texture(ctx.gpu_full_access, POTATO_SPRITE)
    )
    caption_panel_texture = computed(
        lambda: _upload_overlay_into_a_texture(ctx.gpu_full_access, SAMPLE_STREAM_CAPTION_PANEL)
    )
    # Advances only on a published frame, so frame N's picture never depends on a dropped one.
    next_frame_index = 0
    consecutive_dropped_frames = 0

    def note_a_frame_the_output_pool_refused(refusal: RuntimeError) -> None:
        nonlocal consecutive_dropped_frames
        consecutive_dropped_frames += 1
        if (consecutive_dropped_frames - 1) % DROPPED_FRAMES_BETWEEN_REPORTS == 0:
            log.warn(
                f"TestPatternSource: dropped frame {next_frame_index}, "
                f"{consecutive_dropped_frames} in a row: {refusal}"
            )

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal next_frame_index, consecutive_dropped_frames
        frame_width, frame_height = width.get(), height.get()
        try:
            frame_texture = frame_ring.next_texture_for_this_frame(
                ctx.gpu_limited_access, frame_width, frame_height
            )
        except RuntimeError as refusal:
            note_a_frame_the_output_pool_refused(refusal)
            return
        consecutive_dropped_frames = 0
        compute_kernel.dispatch(
            bindings={
                _POTATO_SPRITE_BINDING_NAME: potato_sprite_texture.get(),
                _CAPTION_PANEL_BINDING_NAME: caption_panel_texture.get(),
                _TEST_PATTERN_FRAME_BINDING_NAME: frame_texture,
            },
            group_count=(
                _workgroups_covering(frame_width),
                _workgroups_covering(frame_height),
                1,
            ),
            push_constants=pack_push_constants_for_frame(
                next_frame_index, frame_width, frame_height
            ),
        )
        next_frame_index += 1
        video.write(
            TextureToWrite.of_texture(frame_texture, TEST_PATTERN_FRAME_COLOUR),
            timestamp_ns=monotonic_now_ns(),
        )

    log.info("TestPatternSource: rendering on the GPU")
    return each_frame
