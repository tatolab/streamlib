# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Pixels on the GPU: the effect the stream wires after its source.

Importable as `nodes.halftone_lens_effect:HalftoneLensEffect`, which is the
name the engine spawns this node's child interpreter with.
"""

import struct

from tatolab.stream import (
    Image,
    ImageFrameToWrite,
    Input,
    NodeOutputTextureRing,
    Output,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    node,
)

# The whole effect, as a Slang compute kernel: one invocation per output pixel.
# A lens drifts around the frame and turns what it covers into dots. `dials`
# are the values `process()` pushes each frame. Edit it while `tatolab dev`
# runs to see the change.
HALFTONE_LENS_SLANG = """
[[vk::binding(0, 0)]] Sampler2D source_frame;
[[vk::binding(1, 0)]] [format("rgba8")] RWTexture2D<float4> halftone_frame;

struct HalftoneLensDials {
    float2 frame_size;
    float seconds;
};
[[vk::push_constant]] ConstantBuffer<HalftoneLensDials> dials;

// How big the drifting lens is, as a share of the frame's height.
static const float LENS_RADIUS = 0.3;
// How many pixels apart the dots sit under the lens.
static const float DOT_CELL_SIZE = 8.0;
// How far the gaps between the dots fade toward grey.
static const float GAP_FADE = 0.35;

[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    if (any(float2(invocation.xy) >= dials.frame_size)) {
        return;
    }
    float4 source = source_frame.Load(int3(invocation.xy, 0));
    float2 pixel = float2(invocation.xy) + 0.5;
    float2 lens_centre = dials.frame_size * float2(
        0.5 + 0.3 * sin(dials.seconds * 0.4),
        0.5 + 0.25 * sin(dials.seconds * 0.55)
    );
    float distance_from_lens_centre = length(pixel - lens_centre) / dials.frame_size.y;
    float under_the_lens =
        1.0 - smoothstep(LENS_RADIUS * 0.92, LENS_RADIUS, distance_from_lens_centre);

    // Each cell keeps a round dot of the picture and fades the gap around it
    // toward grey, so the dots show on every colour, black included.
    float2 cell_centre = (floor(pixel / DOT_CELL_SIZE) + 0.5) * DOT_CELL_SIZE;
    float dot_radius = DOT_CELL_SIZE * 0.42;
    float in_the_dot = 1.0 - smoothstep(dot_radius - 1.0, dot_radius, length(pixel - cell_centre));
    float3 gap = lerp(source.rgb, float3(0.5), GAP_FADE);
    float3 dotted = lerp(gap, source.rgb, in_the_dot);

    halftone_frame[invocation.xy] = float4(lerp(source.rgb, dotted, under_the_lens), source.a);
}
"""

# `HalftoneLensDials` as the push constants lay it out: two floats, then one.
HALFTONE_LENS_DIALS = struct.Struct("<fff")

WORKGROUP_TILE_SIZE_IN_PIXELS = 8

NANOSECONDS_PER_SECOND = 1_000_000_000


def workgroups_covering(pixels: int) -> int:
    return (pixels + WORKGROUP_TILE_SIZE_IN_PIXELS - 1) // WORKGROUP_TILE_SIZE_IN_PIXELS


@node
class HalftoneLensEffect:
    """Turns what a slowly drifting lens covers into dots on the GPU, and passes each frame on."""

    video_from_upstream = Input(Image, delivery="newest")
    video_to_downstream = Output(Image)

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.halftone_lens_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=HALFTONE_LENS_SLANG,
            push_constant_size=HALFTONE_LENS_DIALS.size,
            bindings={
                "source_frame": "sampled_texture",
                "halftone_frame": "storage_image",
            },
        )
        # A frame arrives as a pixel buffer, which a kernel cannot sample, so
        # each one is copied into a texture first.
        self.source_frame_textures = NodeOutputTextureRing(
            "rgba8_unorm", ["texture_binding"], depth=1
        )
        self.halftone_frame_textures = NodeOutputTextureRing(
            "rgba8_unorm", ["storage_binding", "texture_binding"]
        )
        self.first_frame_at_ns: int | None = None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(self.video_from_upstream)
        if frame is None:
            return
        # `ctx.time` is stream time in ns, on the clock the stream runs on.
        if self.first_frame_at_ns is None:
            self.first_frame_at_ns = ctx.time
        seconds = (ctx.time - self.first_frame_at_ns) / NANOSECONDS_PER_SECOND

        gpu = ctx.gpu_limited_access
        source_frame = self.source_frame_textures.next_texture_for_this_frame(
            gpu, frame.width, frame.height
        )
        gpu.copy_surface_to_surface(frame.surface_id, source_frame)
        halftone_frame = self.halftone_frame_textures.next_texture_for_this_frame(
            gpu, frame.width, frame.height
        )
        self.halftone_lens_kernel.dispatch(
            bindings={"source_frame": source_frame, "halftone_frame": halftone_frame},
            group_count=(
                workgroups_covering(frame.width),
                workgroups_covering(frame.height),
                1,
            ),
            push_constants=HALFTONE_LENS_DIALS.pack(frame.width, frame.height, seconds),
        )
        ctx.outputs.write(
            self.video_to_downstream,
            ImageFrameToWrite.of_texture(halftone_frame, frame.colour),
            timestamp_ns=frame.timestamp_ns,
        )
