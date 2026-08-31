# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A halftone dot screen, as a GLSL compute kernel the engine dispatches.

Newsprint's trick: divide the picture into cells, and print one ink dot per
cell whose size is that cell's brightness. Light areas get fat dots that touch
their neighbours, dark ones get specks, and the eye reads the average.

The kernel is an object: built once in `setup()`, where the capability is
Full, and dispatched per frame in `process()` with its bindings passed by
name. The GLSL below is a Python string until the engine compiles it, and the
compiler ships inside the wheel — there is no shader toolchain here.

Every invocation reads a texel it does not write — its cell's centre — which
is what separates this from a per-texel grade and why the effect needs two
textures rather than one edited in place.

The one step that is not the kernel is the landing copy. `CameraSource`
publishes a buffer-backed frame and a dispatch binds texture-backed surfaces
only, so each frame is copied device-to-device into a texture this processor
owns before the kernel can sample it. cupy does nothing in this module but
that copy; the pixels never touch the host.
"""

from __future__ import annotations

import math
import struct

import cupy

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    ProcessorOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    output,
    processor,
)

CAMERA_FRAME_INPUT_PORT = "camera_frame_from_upstream"
HALFTONE_FRAME_OUTPUT_PORT = "halftone_frame_to_downstream"

# The shader's own names for its two bindings, taken off it by reflection at
# construction. A dispatch resolves against these and never against slot
# order, so they are the contract between the GLSL below and the `dispatch`
# call — and they are deliberately not the port names above, because a port
# and a shader binding are two different contracts that happen to carry the
# same frame.
CAMERA_FRAME_BINDING = "camera_frame"
HALFTONE_FRAME_BINDING = "halftone_frame"

# One format end to end, so no stage in the chain has to convert. The camera
# publishes RGBA8, the kernel samples it and the window shows the result.
TEXTURE_FORMAT = "rgba8_unorm"

# The texture the landing copy fills and the kernel samples. `copy_src` and
# `copy_dst` ride every acquire, which is what lets the copy write into it.
SAMPLED_TEXTURE_USAGE = ["texture_binding"]

# The texture the kernel writes as a storage image and the window then samples.
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

# The dispatch asks for one workgroup per tile of this size, and the shader
# declares the same size as its `local_size`. The two reach the shader as one
# number — see the `#define` below — because raising the Python without the
# GLSL leaves the right and bottom of every frame unscreened and nothing
# anywhere refuses. It has nothing to do with the halftone cell size, which is
# a runtime dial: a tile is how the GPU is diced up, a cell is how the picture
# is.
WORKGROUP_TILE_SIZE = 8

# `int cell_size; float dot_boost; float background_level;` — three 4-byte
# scalars, so std430 packs them at offsets 0, 4 and 8 with no padding, and
# little-endian at the wire like every push constant.
HALFTONE_DIAL_FORMAT = "<iff"
HALFTONE_DIAL_SIZE = struct.calcsize(HALFTONE_DIAL_FORMAT)

# A near-black paper rather than a white page, which is what makes this read
# as a lit sign instead of a newspaper; the boost lifts a dot far enough off it
# to look saturated.
DEFAULT_CELL_SIZE = 8
DEFAULT_DOT_BOOST = 1.3
DEFAULT_BACKGROUND_LEVEL = 0.0627

# The `#define` is the only interpolated line: the body stays a plain string,
# so the shader's own braces need no doubling and it reads as GLSL.
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
    // BT.709 luma, the weights the HD standard published for it.
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


@processor(description="Screens each frame into halftone dots with a compute kernel")
class HalftoneCompute:
    """Camera frame in, the same picture as a screen of ink dots out."""

    def __init__(
        self,
        cell_size: int = DEFAULT_CELL_SIZE,
        dot_boost: float = DEFAULT_DOT_BOOST,
        background_level: float = DEFAULT_BACKGROUND_LEVEL,
    ) -> None:
        if int(cell_size) < 2:
            raise ValueError(
                f"HalftoneCompute was configured with cell_size={cell_size} — a "
                f"cell holds one dot and needs at least 2 pixels across to draw "
                f"one; 8 is the screen the effect was written for"
            )
        if not math.isfinite(float(dot_boost)) or float(dot_boost) <= 0.0:
            raise ValueError(
                f"HalftoneCompute was configured with dot_boost={dot_boost} — the "
                f"dial scales the ink a dot is drawn in, so it wants a finite "
                f"number above 0.0: 1.0 leaves the sampled colour alone, at or "
                f"below 0.0 prints black dots on the background, and a NaN or an "
                f"infinity reaches the shader as a multiply with no defined result"
            )
        if not 0.0 <= float(background_level) <= 1.0:
            raise ValueError(
                f"HalftoneCompute was configured with "
                f"background_level={background_level} — the dial is the grey the "
                f"paper is, from 0.0 (black) to 1.0 (white)"
            )
        # Packed once, because the dials are fixed at construction. They are
        # still handed to every dispatch below: push constants travel with a
        # dispatch and never persist on the kernel, exactly as bindings do.
        self.halftone_dial_push_constants = struct.pack(
            HALFTONE_DIAL_FORMAT,
            int(cell_size),
            float(dot_boost),
            float(background_level),
        )

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Depth 1, unlike the output ring: `dispatch` returns with the GPU
        # work retired, and nothing outside this processor ever names this
        # texture, so the frame it holds is finished with before the next one
        # lands in it.
        self.camera_frame_landing_ring = ProcessorOutputTextureRing(
            TEXTURE_FORMAT, SAMPLED_TEXTURE_USAGE, depth=1
        )
        self.halftone_frame_ring = ProcessorOutputTextureRing(
            TEXTURE_FORMAT, STORAGE_AND_SAMPLED_TEXTURE_USAGE
        )
        self.halftone_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=HALFTONE_COMPUTE_GLSL,
            push_constant_size=HALFTONE_DIAL_SIZE,
            # Asserted against the shader's own reflection, so renaming a
            # binding on one side of this file is refused here at construction
            # rather than at the first dispatch.
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
        # The frame is a DLPack producer in its own right, so this is the whole
        # read — GPU-resident, and the cast object's claim is what holds the
        # camera's pixels still for the length of the copy. Leaving the scope
        # blits the write into the texture, ordered ahead of the engine's next
        # read of it.
        with camera_frame_landing_texture.as_device_tensor() as writable_texture:
            cupy.from_dlpack(writable_texture)[...] = cupy.from_dlpack(frame)

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

        ctx.outputs.write(
            HALFTONE_FRAME_OUTPUT_PORT,
            {
                "surface_id": halftone_frame_texture.surface_id,
                "width": frame.width,
                "height": frame.height,
                # Carried from the frame this one was derived from rather than
                # re-read off the clock: the timestamp is the ordering
                # primitive downstream, and restamping here would date the
                # picture to when the effect ran instead of when the camera
                # captured it.
                "timestamp_ns": frame.timestamp_ns,
            },
        )

    @input(
        delivery_profile="newest",
        description="Frames from the camera, as VideoFrame bags",
    )
    def camera_frame_from_upstream(self) -> VideoFrame: ...

    @output(description="The same frames screened into halftone dots by the kernel")
    def halftone_frame_to_downstream(self) -> VideoFrame: ...
