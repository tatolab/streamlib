# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A halftone dot screen, as a Slang compute kernel the runtime dispatches.

Importable as `nodes.halftone_compute:HalftoneCompute`, which is the name the
engine spawns this node's child interpreter with.
"""

import struct
from typing import Annotated

from tatolab.stream import (
    Description,
    ImageFrameToWrite,
    InputPin,
    NodeOutputTextureRing,
    NumberRange,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    node,
)

# The shader's own names for its two bindings, read off it by reflection. A
# dispatch resolves against these, never against slot order or port names.
CAMERA_FRAME_BINDING = "camera_frame"
HALFTONE_FRAME_BINDING = "halftone_frame"

# One format end to end: a camera frame lands as RGBA8, the kernel writes RGBA8.
TEXTURE_FORMAT = "rgba8_unorm"

# The texture the landing copy fills and the kernel samples. `copy_src` and
# `copy_dst` ride every acquire, which is what lets the copy write into it.
SAMPLED_TEXTURE_USAGE = ["texture_binding"]

# The texture the kernel writes as a storage image and downstream then reads.
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

# Reaches the shader as its `numthreads` through the constant below, so the
# dispatch's tile and the shader's cannot disagree. Unrelated to the halftone
# cell: a tile is how the GPU is diced up, a cell is how the picture is.
WORKGROUP_TILE_SIZE = 8

# `int cell_size; float dot_boost; float background_level;` — three 4-byte
# scalars, which a push constant packs at offsets 0, 4 and 8 with no padding,
# little-endian like every push constant.
HALFTONE_DIAL_FORMAT = "<iff"
HALFTONE_DIAL_SIZE = struct.calcsize(HALFTONE_DIAL_FORMAT)

DEFAULT_CELL_SIZE = 8
DEFAULT_DOT_BOOST = 1.3
DEFAULT_BACKGROUND_LEVEL = 0.0627

HALFTONE_COMPUTE_SLANG = f"""
static const uint WORKGROUP_TILE_SIZE = {WORKGROUP_TILE_SIZE};
"""  """
[[vk::binding(0, 0)]] Sampler2D camera_frame;
[[vk::binding(1, 0)]] [format("rgba8")] RWTexture2D<float4> halftone_frame;

struct HalftoneDial {
    int cell_size;
    float dot_boost;
    float background_level;
};
[[vk::push_constant]] ConstantBuffer<HalftoneDial> dial;

[numthreads(WORKGROUP_TILE_SIZE, WORKGROUP_TILE_SIZE, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    int2 at = int2(invocation.xy);
    uint2 extent_unsigned;
    halftone_frame.GetDimensions(extent_unsigned.x, extent_unsigned.y);
    int2 extent = int2(extent_unsigned);
    // The dispatch rounds up to whole workgroups, so the tiles along the right
    // and bottom edges run past a frame whose extent is not a multiple of the
    // tile. Those invocations have no texel to write.
    if (at.x >= extent.x || at.y >= extent.y) {
        return;
    }

    // The cell this texel sits in, and the texel at that cell's centre. Every
    // invocation in a cell reads the same one, so the whole cell is drawn from
    // a single sample of the picture — that gather is the halftone. Clamped
    // because a cell hanging off the right or bottom edge has its centre
    // outside the frame.
    int2 cell = at / dial.cell_size;
    int2 centre = min(cell * dial.cell_size + dial.cell_size / 2, extent - 1);

    // Load rather than Sample: the centre is an exact texel index, so there is
    // nothing to filter.
    float4 ink = camera_frame.Load(int3(centre, 0));
    // BT.709 luma.
    float luma = dot(ink.rgb, float3(0.2126, 0.7152, 0.0722));

    // Tone is carried by how much of the cell the ink covers, so it is the
    // dot's *area* that scales with luma and the radius that takes the square
    // root. A radius scaled by luma directly loses the shadows: at a quarter
    // grey it inks a single texel of the sixty-four.
    //
    // 0.55 of the cell rather than 0.5, so dots in adjacent cells just touch
    // at full luma instead of leaving a permanent grid of background between
    // them.
    float radius = float(dial.cell_size) * 0.55 * sqrt(luma);
    float distance_from_centre = distance(float2(at), float2(centre));
    // One texel of feather on the dot's edge. A hard cutoff crawls on moving
    // video: a radius that grows by a fraction of a texel per frame lands on
    // an aliased edge that jumps a whole texel at a time.
    float coverage = 1.0 - smoothstep(radius - 1.0, radius, distance_from_centre);

    float3 dot_colour = min(ink.rgb * dial.dot_boost, float3(1.0));
    float3 paper = float3(dial.background_level);
    // Opaque, and not the sampled alpha: that one belongs to the cell's centre
    // texel, so carrying it would quantise transparency to the dot grid.
    halftone_frame[invocation.xy] = float4(lerp(paper, dot_colour, coverage), 1.0);
}
"""


def _workgroups_covering(pixels: int) -> int:
    """How many tiles it takes to cover `pixels`, the last one hanging over."""
    return (pixels + WORKGROUP_TILE_SIZE - 1) // WORKGROUP_TILE_SIZE


@node.meta(description="Screens each camera frame into halftone dots with a compute kernel")
@node
class HalftoneCompute:
    """Camera frame in, the same picture as a halftone dot screen out."""

    cell_size: Annotated[
        int,
        NumberRange(minimum=2, maximum=64),
        Description("Pixels across one dot cell; bigger is a coarser screen."),
    ] = DEFAULT_CELL_SIZE
    dot_boost: Annotated[
        float,
        NumberRange(minimum=0.01, maximum=4.0),
        Description("How far a dot's ink is lifted above the colour it sampled."),
    ] = DEFAULT_DOT_BOOST
    background_level: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=1.0),
        Description("The grey the paper is, from 0.0 (black) to 1.0 (white)."),
    ] = DEFAULT_BACKGROUND_LEVEL
    camera_frame_from_upstream: InputPin[
        Annotated[Texture, StreamDelivery("newest"), Description("Frames from the camera")]
    ]
    halftone_frame_to_downstream: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(TEXTURE_FORMAT),
            Description("The same frames screened into halftone dots by the kernel"),
        ]
    ]

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Depth 1: `dispatch` returns with the GPU work retired, and nothing
        # outside this node ever names the landing texture.
        self.camera_frame_landing_ring = NodeOutputTextureRing(
            TEXTURE_FORMAT, SAMPLED_TEXTURE_USAGE, depth=1
        )
        self.halftone_frame_ring = NodeOutputTextureRing(
            TEXTURE_FORMAT, STORAGE_AND_SAMPLED_TEXTURE_USAGE
        )
        self.halftone_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=HALFTONE_COMPUTE_SLANG,
            push_constant_size=HALFTONE_DIAL_SIZE,
            # Asserted against the shader's own reflection, so a binding renamed
            # on one side of this file is refused here, before the first frame.
            bindings={
                CAMERA_FRAME_BINDING: "sampled_texture",
                HALFTONE_FRAME_BINDING: "storage_image",
            },
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = self.camera_frame_from_upstream.read()
        if frame is None:
            return

        camera_frame_landing_texture = (
            self.camera_frame_landing_ring.next_texture_for_this_frame(
                ctx.gpu_limited_access, frame.width, frame.height
            )
        )
        # `frame`'s claim holds the camera's pixels still for the copy, which
        # returns once the kernel's read of the texture would see them.
        ctx.gpu_limited_access.copy_surface_to_surface(
            frame.surface_id, camera_frame_landing_texture
        )

        halftone_frame_texture = self.halftone_frame_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        self.halftone_kernel.dispatch(
            bindings={
                CAMERA_FRAME_BINDING: camera_frame_landing_texture,
                HALFTONE_FRAME_BINDING: halftone_frame_texture,
            },
            group_count=(
                _workgroups_covering(frame.width),
                _workgroups_covering(frame.height),
                1,
            ),
            # Push constants travel with a dispatch and never persist on the
            # kernel, so the dials as they stand now go with each one.
            push_constants=struct.pack(
                HALFTONE_DIAL_FORMAT, self.cell_size, self.dot_boost, self.background_level
            ),
        )

        # The capture's timestamp, not the effect's: downstream orders frames by
        # when the camera captured them.
        self.halftone_frame_to_downstream.write(
            ImageFrameToWrite.of_texture(halftone_frame_texture, frame.colour),
            timestamp_ns=frame.timestamp_ns,
        )
