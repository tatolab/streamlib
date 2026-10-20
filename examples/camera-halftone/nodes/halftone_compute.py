# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A halftone dot screen, as a GLSL compute kernel the engine dispatches.

Importable as `nodes.halftone_compute:HalftoneCompute`, which is the name the
engine spawns this node's child interpreter with.
"""

import math
import struct
from dataclasses import dataclass

from tatolab.stream import (
    NodeOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    node,
)

CAMERA_FRAME_INPUT_PORT = "camera_frame_from_upstream"
HALFTONE_FRAME_OUTPUT_PORT = "halftone_frame_to_downstream"

# The shader's own names for its two bindings, read off it by reflection. A
# dispatch resolves against these, never against slot order or port names.
CAMERA_FRAME_BINDING = "camera_frame"
HALFTONE_FRAME_BINDING = "halftone_frame"

# One format end to end: a camera frame lands as RGBA8, the kernel writes RGBA8.
TEXTURE_FORMAT = "rgba8_unorm"

# The texture the landing copy fills and the kernel samples. `copy_src` and
# `copy_dst` ride every acquire, which is what lets the copy write into it.
SAMPLED_TEXTURE_USAGE = ["texture_binding"]

# The texture the kernel writes as a storage image and the encoder then reads.
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

# Reaches the shader as its `local_size` through the `#define` below, so the
# dispatch's tile and the shader's cannot disagree. Unrelated to the halftone
# cell: a tile is how the GPU is diced up, a cell is how the picture is.
WORKGROUP_TILE_SIZE = 8

# `int cell_size; float dot_boost; float background_level;` — three 4-byte
# scalars, which std430 packs at offsets 0, 4 and 8 with no padding,
# little-endian like every push constant.
HALFTONE_DIAL_FORMAT = "<iff"
HALFTONE_DIAL_SIZE = struct.calcsize(HALFTONE_DIAL_FORMAT)

DEFAULT_CELL_SIZE = 8
DEFAULT_DOT_BOOST = 1.3
DEFAULT_BACKGROUND_LEVEL = 0.0627

HALFTONE_COMPUTE_GLSL = (
    f"#version 450\n#define WORKGROUP_TILE_SIZE {WORKGROUP_TILE_SIZE}\n"
    """
layout(local_size_x = WORKGROUP_TILE_SIZE, local_size_y = WORKGROUP_TILE_SIZE) in;

layout(set = 0, binding = 0) uniform sampler2D camera_frame;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D halftone_frame;

layout(push_constant) uniform HalftoneDial {
    int cell_size;
    float dot_boost;
    float background_level;
} dial;

void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    ivec2 extent = imageSize(halftone_frame);
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
    ivec2 cell = at / dial.cell_size;
    ivec2 centre = min(cell * dial.cell_size + dial.cell_size / 2, extent - 1);

    // texelFetch rather than texture(): the centre is an exact texel index, so
    // there is nothing to filter.
    vec4 ink = texelFetch(camera_frame, centre, 0);
    // BT.709 luma.
    float luma = dot(ink.rgb, vec3(0.2126, 0.7152, 0.0722));

    // Tone is carried by how much of the cell the ink covers, so it is the
    // dot's *area* that scales with luma and the radius that takes the square
    // root. A radius scaled by luma directly loses the shadows: at a quarter
    // grey it inks a single texel of the sixty-four.
    //
    // 0.55 of the cell rather than 0.5, so dots in adjacent cells just touch
    // at full luma instead of leaving a permanent grid of background between
    // them.
    float radius = float(dial.cell_size) * 0.55 * sqrt(luma);
    float distance_from_centre = distance(vec2(at), vec2(centre));
    // One texel of feather on the dot's edge. A hard cutoff crawls on moving
    // video: a radius that grows by a fraction of a texel per frame lands on
    // an aliased edge that jumps a whole texel at a time.
    float coverage = 1.0 - smoothstep(radius - 1.0, radius, distance_from_centre);

    vec3 dot_colour = min(ink.rgb * dial.dot_boost, vec3(1.0));
    vec3 paper = vec3(dial.background_level);
    // Opaque, and not the sampled alpha: that one belongs to the cell's centre
    // texel, so carrying it would quantise transparency to the dot grid.
    imageStore(halftone_frame, at, vec4(mix(paper, dot_colour, coverage), 1.0));
}
"""
)


def _workgroups_covering(pixels: int) -> int:
    """How many tiles it takes to cover `pixels`, the last one hanging over."""
    return (pixels + WORKGROUP_TILE_SIZE - 1) // WORKGROUP_TILE_SIZE


@dataclass
class HalftoneComputeConfig:
    """HalftoneCompute's settings, as `stream_builder.add(..., config={...})` spells them."""

    cell_size: int = DEFAULT_CELL_SIZE
    """Pixels across one dot cell; bigger is a coarser screen. At least 2."""

    dot_boost: float = DEFAULT_DOT_BOOST
    """How far a dot's ink is lifted above the colour it sampled; finite and above 0.0."""

    background_level: float = DEFAULT_BACKGROUND_LEVEL
    """The grey the paper is, from 0.0 (black) to 1.0 (white)."""


@node
class HalftoneCompute:
    """Screens each camera frame into halftone dots with a compute kernel."""

    @node.input(
        delivery_profile="newest",
        description="Frames from the camera, as VideoFrame records",
    )
    def camera_frame_from_upstream(self) -> VideoFrame: ...

    @node.output(
        description="The same frames screened into halftone dots by the kernel"
    )
    def halftone_frame_to_downstream(self) -> VideoFrame: ...

    def __init__(self, config: HalftoneComputeConfig) -> None:
        cell_size = int(config.cell_size)
        dot_boost = float(config.dot_boost)
        background_level = float(config.background_level)
        if cell_size < 2:
            raise ValueError(
                f"HalftoneCompute was configured with cell_size={cell_size} — a "
                f"cell holds one dot and needs at least 2 pixels across to draw "
                f"one; 8 is the screen the effect was written for"
            )
        if not math.isfinite(dot_boost) or dot_boost <= 0.0:
            raise ValueError(
                f"HalftoneCompute was configured with dot_boost={dot_boost} — the "
                f"dial scales the ink a dot is drawn in, so it wants a finite "
                f"number above 0.0: 1.0 leaves the sampled colour alone, at or "
                f"below 0.0 prints black dots on the background, and a NaN or an "
                f"infinity reaches the shader as a multiply with no defined result"
            )
        if not 0.0 <= background_level <= 1.0:
            raise ValueError(
                f"HalftoneCompute was configured with "
                f"background_level={background_level} — the dial is the grey the "
                f"paper is, from 0.0 (black) to 1.0 (white)"
            )
        # Push constants travel with a dispatch and never persist on the
        # kernel, so these bytes are handed to every dispatch.
        self.halftone_dial_push_constants = struct.pack(
            HALFTONE_DIAL_FORMAT, cell_size, dot_boost, background_level
        )

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
            source=HALFTONE_COMPUTE_GLSL,
            push_constant_size=HALFTONE_DIAL_SIZE,
            # Asserted against the shader's own reflection, so a binding renamed
            # on one side of this file is refused here, before the first frame.
            bindings={
                CAMERA_FRAME_BINDING: "sampled_texture",
                HALFTONE_FRAME_BINDING: "storage_image",
            },
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(CAMERA_FRAME_INPUT_PORT, into=VideoFrame)
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
            push_constants=self.halftone_dial_push_constants,
        )

        # The capture's timestamp, not the effect's: downstream orders frames by
        # when the camera captured them.
        halftone_frame = VideoFrame(
            surface_id=halftone_frame_texture.surface_id,
            width=frame.width,
            height=frame.height,
            timestamp_ns=frame.timestamp_ns,
            color_info=frame.color_info,
        )
        ctx.outputs.write(
            HALFTONE_FRAME_OUTPUT_PORT,
            halftone_frame.to_bytes(),
            timestamp_ns=frame.timestamp_ns,
        )
