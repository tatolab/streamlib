# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The two halves, cut down the middle by a compute kernel, and labelled.

Importable as `nodes.split_screen_compositor:SplitScreenCompositor`, which is
the name the runtime spawns this node's child interpreter with. Two producers
fan into it, and the dispatch binds both upstream surface ids straight into the
shader: each is a kernel output, texture-backed, so it needs no landing copy.

The labels are generated into the Slang as a `static const` bitmap, so the kernel
source is a function of `left_label` and `right_label`.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass

from tatolab.stream import (
    Image,
    ImageFrame,
    ImageFrameToWrite,
    Input,
    NodeOutputTextureRing,
    Output,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    node,
)

from nodes.label_font import (
    GLYPH_HEIGHT,
    label_pixel_rows,
    label_width_in_pixels,
)

# The shader's own names for its three bindings, read off it by reflection.
RASTERIZED_FRAME_BINDING = "rasterized_frame"
RAY_TRACED_FRAME_BINDING = "ray_traced_frame"
SPLIT_SCREEN_FRAME_BINDING = "split_screen_frame"

TEXTURE_FORMAT = "rgba8_unorm"
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

WORKGROUP_TILE_SIZE = 8

# `float split_fraction`, little-endian like every push constant.
SPLIT_PUSH_CONSTANT_FORMAT = "<f"
SPLIT_PUSH_CONSTANT_SIZE = struct.calcsize(SPLIT_PUSH_CONSTANT_FORMAT)

DEFAULT_LEFT_LABEL = "RTX OFF"
DEFAULT_RIGHT_LABEL = "RTX ON"

# Label geometry relative to the frame, so a label keeps its proportions at
# any resolution: one font pixel per this many frame pixels across, and the top
# margin as a fraction of the height.
FRAME_PIXELS_PER_FONT_PIXEL = 160
FRAME_PIXELS_PER_TOP_MARGIN = 26

_LABEL_BITMAP_WORD_BITS = 32


def _packed_label_bitmap(labels: tuple[str, str]) -> tuple[int, int, list[int]]:
    """Both labels stacked into one array: `(width, words_per_row, words)`.

    The narrower label is padded, so the shader needs one lookup and one index.
    """
    width = max(label_width_in_pixels(label) for label in labels)
    words_per_row = max(1, -(-width // _LABEL_BITMAP_WORD_BITS))
    words: list[int] = []
    for label in labels:
        for row_bits in label_pixel_rows(label):
            for word in range(words_per_row):
                words.append(
                    (row_bits >> (word * _LABEL_BITMAP_WORD_BITS))
                    & ((1 << _LABEL_BITMAP_WORD_BITS) - 1)
                )
    return width, words_per_row, words


def _split_screen_compute_slang(left_label: str, right_label: str) -> str:
    """The kernel source for one compositor, its labels generated into it."""
    _, words_per_row, words = _packed_label_bitmap((left_label, right_label))
    label_widths = ", ".join(
        str(label_width_in_pixels(label)) for label in (left_label, right_label)
    )
    packed_words = ", ".join(f"{word}u" for word in words)
    # Only the constants and the two arrays are interpolated; the body stays a
    # plain string, so the shader's braces need no doubling.
    return (
        f"static const uint WORKGROUP_TILE_SIZE = {WORKGROUP_TILE_SIZE};\n"
        f"static const int DIVIDER_HALF_WIDTH_IN_PIXELS = 2;\n"
        f"static const float4 DIVIDER_COLOUR = float4(0.92, 0.94, 0.98, 1.0);\n"
        f"static const float4 LABEL_INK_COLOUR = float4(1.0, 1.0, 1.0, 1.0);\n"
        f"static const float4 LABEL_OUTLINE_COLOUR = float4(0.0, 0.0, 0.0, 1.0);\n"
        f"static const int LABEL_HEIGHT = {GLYPH_HEIGHT};\n"
        f"static const int LABEL_BITMAP_WORDS_PER_ROW = {words_per_row};\n"
        f"static const int FRAME_PIXELS_PER_FONT_PIXEL = {FRAME_PIXELS_PER_FONT_PIXEL};\n"
        f"static const int FRAME_PIXELS_PER_TOP_MARGIN = {FRAME_PIXELS_PER_TOP_MARGIN};\n"
        f"static const int LABEL_WIDTHS[2] = {{ {label_widths} }};\n"
        f"static const uint LABEL_BITMAP[{len(words)}] = {{ {packed_words} }};\n"
        """
[[vk::binding(0, 0)]] Sampler2D rasterized_frame;
[[vk::binding(1, 0)]] Sampler2D ray_traced_frame;
[[vk::binding(2, 0)]] [format("rgba8")] RWTexture2D<float4> split_screen_frame;

struct SplitDial {
    float split_fraction;
};
[[vk::push_constant]] ConstantBuffer<SplitDial> dial;

// Whether font pixel (x, y) of `label` is ink. Bit 0 of a word is its
// leftmost pixel, the order Python packed them in.
bool label_ink(int label, int x, int y) {
    if (x < 0 || y < 0 || x >= LABEL_WIDTHS[label] || y >= LABEL_HEIGHT) {
        return false;
    }
    int row = label * LABEL_HEIGHT + y;
    uint word = LABEL_BITMAP[row * LABEL_BITMAP_WORDS_PER_ROW + (x >> 5)];
    return (word & (1u << uint(x & 31))) != 0u;
}

// The same test in frame pixels. Negatives are rejected before the division,
// which truncates towards zero and would fold the column left of a label onto
// its column 0.
bool label_ink_at(int label, int2 origin, int scale, int2 at) {
    int2 within = at - origin;
    if (within.x < 0 || within.y < 0) {
        return false;
    }
    return label_ink(label, within.x / scale, within.y / scale);
}

// Centred over its own half of the frame, near the top.
int2 label_origin(int label, int2 extent, int divide_at, int scale) {
    int half_start = (label == 0) ? 0 : divide_at;
    int half_width = (label == 0) ? divide_at : (extent.x - divide_at);
    return int2(half_start + (half_width - LABEL_WIDTHS[label] * scale) / 2,
                extent.y / FRAME_PIXELS_PER_TOP_MARGIN);
}

[numthreads(WORKGROUP_TILE_SIZE, WORKGROUP_TILE_SIZE, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    int2 at = int2(invocation.xy);
    uint2 extent_unsigned;
    split_screen_frame.GetDimensions(extent_unsigned.x, extent_unsigned.y);
    int2 extent = int2(extent_unsigned);
    // The dispatch rounds up to whole workgroups, so the edge tiles run past a
    // frame whose extent is not a multiple of the tile.
    if (at.x >= extent.x || at.y >= extent.y) {
        return;
    }

    int divide_at = int(float(extent.x) * dial.split_fraction);
    // All three surfaces share one extent, so the load reads the exact texel.
    float4 colour = at.x < divide_at
        ? rasterized_frame.Load(int3(at, 0))
        : ray_traced_frame.Load(int3(at, 0));
    if (abs(at.x - divide_at) <= DIVIDER_HALF_WIDTH_IN_PIXELS) {
        colour = DIVIDER_COLOUR;
    }

    // Labels last, over both halves and the divider: white ink, outlined in
    // black by testing the glyph at eight offsets around this pixel.
    int scale = max(2, extent.x / FRAME_PIXELS_PER_FONT_PIXEL);
    int outline = max(1, scale / 3);
    for (int label = 0; label < 2; label++) {
        int2 origin = label_origin(label, extent, divide_at, scale);
        if (label_ink_at(label, origin, scale, at)) {
            colour = LABEL_INK_COLOUR;
            break;
        }
        bool touching_ink = false;
        for (int down = -1; down <= 1; down++) {
            for (int across = -1; across <= 1; across++) {
                touching_ink = touching_ink
                    || label_ink_at(label, origin, scale,
                                    at + int2(across, down) * outline);
            }
        }
        if (touching_ink) {
            colour = LABEL_OUTLINE_COLOUR;
            break;
        }
    }

    split_screen_frame[invocation.xy] = colour;
}
"""
    )


def _workgroups_covering(pixels: int) -> int:
    """How many tiles it takes to cover `pixels`, the last one hanging over."""
    return (pixels + WORKGROUP_TILE_SIZE - 1) // WORKGROUP_TILE_SIZE


@dataclass
class SplitScreenCompositorConfig:
    """SplitScreenCompositor's settings: where the cut falls and what each half is labelled."""

    split_fraction: float = 0.5
    left_label: str = DEFAULT_LEFT_LABEL
    right_label: str = DEFAULT_RIGHT_LABEL


@node.meta(description="Cuts the rasterized and ray-traced frames together")
@node.process
class SplitScreenCompositor:
    """Rasterized on the left, ray traced on the right, one labelled frame out."""

    def __init__(self, config: SplitScreenCompositorConfig) -> None:
        split_fraction = float(config.split_fraction)
        if not 0.0 <= split_fraction <= 1.0:
            raise ValueError(
                f"SplitScreenCompositor was configured with "
                f"split_fraction={split_fraction} — the dial runs from 0.0 (all "
                f"ray traced) to 1.0 (all rasterized), and 0.5 cuts down the middle"
            )
        self.left_label = config.left_label
        self.right_label = config.right_label
        # Push constants travel with each dispatch and never persist on the
        # kernel, so this is handed to every one.
        self.split_push_constants = struct.pack(
            SPLIT_PUSH_CONSTANT_FORMAT, split_fraction
        )
        self.newest_rasterized_frame: ImageFrame | None = None

    rasterized_frame_from_upstream = Input(
        Image, delivery="newest", description="The rasterized half — direct lighting only"
    )
    ray_traced_frame_from_upstream = Input(
        Image,
        delivery="newest",
        description="The ray-traced half — shadows and a mirror floor",
    )
    split_screen_frame_to_downstream = Output(
        Image, description="Rasterized left, ray traced right, labelled and cut"
    )

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.output_ring = NodeOutputTextureRing(
            TEXTURE_FORMAT, STORAGE_AND_SAMPLED_TEXTURE_USAGE
        )
        self.split_screen_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=_split_screen_compute_slang(self.left_label, self.right_label),
            push_constant_size=SPLIT_PUSH_CONSTANT_SIZE,
            bindings={
                RASTERIZED_FRAME_BINDING: "sampled_texture",
                RAY_TRACED_FRAME_BINDING: "sampled_texture",
                SPLIT_SCREEN_FRAME_BINDING: "storage_image",
            },
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        # Held as the frame the read returned, so its surface stays claimed
        # against the rasterizer reusing it until a newer one replaces it.
        rasterized_frame = ctx.inputs.read(self.rasterized_frame_from_upstream)
        if rasterized_frame is not None:
            self.newest_rasterized_frame = rasterized_frame

        # The traced half paces the composite: a wake that brought only a
        # rasterized frame holds it for the next traced one, which costs one
        # dispatch per composite and leaves the left half at most one frame
        # interval behind the right.
        ray_traced_frame = ctx.inputs.read(self.ray_traced_frame_from_upstream)
        if ray_traced_frame is None or self.newest_rasterized_frame is None:
            return

        width = ray_traced_frame.width
        height = ray_traced_frame.height
        split_screen_frame_texture = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, width, height
        )
        self.split_screen_kernel.dispatch(
            bindings={
                RASTERIZED_FRAME_BINDING: self.newest_rasterized_frame.surface_id,
                RAY_TRACED_FRAME_BINDING: ray_traced_frame.surface_id,
                SPLIT_SCREEN_FRAME_BINDING: split_screen_frame_texture,
            },
            group_count=(
                _workgroups_covering(width),
                _workgroups_covering(height),
                1,
            ),
            push_constants=self.split_push_constants,
        )

        # Stamped with the traced frame's render instant, not the moment of
        # the cut: downstream orders by it.
        ctx.outputs.write(
            self.split_screen_frame_to_downstream,
            ImageFrameToWrite.of_texture(split_screen_frame_texture, ray_traced_frame.colour),
            timestamp_ns=ray_traced_frame.timestamp_ns,
        )
