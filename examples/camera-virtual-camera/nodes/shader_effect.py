# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""One effect host, any number of looks: a Slang `effect` function taken from config.

Importable as `nodes.shader_effect:ShaderEffect`, which is the name the runtime
spawns this node's child interpreter with — and the `type` an agent passes to
`add_node` to put a look into the running stream.

The host is written once. A look is the `float4 effect(float4 source, int2 at)`
function a config carries: the host wraps it in a compute kernel at `setup()`
and runs it over every frame on the GPU, so the pixels never leave it.
`looks/` holds three to start from.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

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

SHIPPED_LOOKS_DIRECTORY = Path(__file__).parent / "looks"

SOURCE_FRAME_BINDING = "shader_effect_source_frame"
LOOK_FRAME_BINDING = "shader_effect_look_frame"

TEXTURE_FORMAT = "rgba8_unorm"

# The texture the landing copy fills and the kernel samples. `copy_src` and
# `copy_dst` ride every acquire, which is what lets the copy write into it.
SAMPLED_TEXTURE_USAGE = ["texture_binding"]

# The texture the kernel writes as a storage image and downstream then reads.
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

# The dispatch's tile and the kernel's `numthreads` must agree, so the Slang
# takes this number from here.
WORKGROUP_TILE_SIZE = 8

# What a look may call besides its own code: the frame's extent, and any source
# pixel clamped to the frame's edge.
LOOK_HOST_PRELUDE_SLANG = f"""
static const uint WORKGROUP_TILE_SIZE = {WORKGROUP_TILE_SIZE};
"""  f"""
[[vk::binding(0, 0)]] Sampler2D {SOURCE_FRAME_BINDING};
[[vk::binding(1, 0)]] [format("rgba8")] RWTexture2D<float4> {LOOK_FRAME_BINDING};

int2 shader_effect_frame_extent() {{
    uint2 extent;
    {LOOK_FRAME_BINDING}.GetDimensions(extent.x, extent.y);
    return int2(extent);
}}

float4 shader_effect_source_at(int2 at) {{
    int2 clamped = clamp(at, int2(0), shader_effect_frame_extent() - 1);
    return {SOURCE_FRAME_BINDING}.Load(int3(clamped, 0));
}}
"""

LOOK_HOST_ENTRY_POINT_SLANG = f"""
[numthreads(WORKGROUP_TILE_SIZE, WORKGROUP_TILE_SIZE, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {{
    int2 at = int2(invocation.xy);
    int2 extent = shader_effect_frame_extent();
    // The dispatch rounds up to whole workgroups, so the edge tiles of a frame
    // whose extent is not a multiple of the tile run past it.
    if (at.x >= extent.x || at.y >= extent.y) {{
        return;
    }}
    {LOOK_FRAME_BINDING}[invocation.xy] = effect(shader_effect_source_at(at), at);
}}
"""


def shipped_look_slang(look_file_name: str) -> str:
    """The `effect` function of one look in `looks/`, e.g. `vignette.slang`."""
    return (SHIPPED_LOOKS_DIRECTORY / look_file_name).read_text(encoding="utf-8")


def look_kernel_slang(effect_slang: str) -> str:
    """The whole compute kernel that runs one look's `effect` function."""
    return LOOK_HOST_PRELUDE_SLANG + effect_slang + LOOK_HOST_ENTRY_POINT_SLANG


def _workgroups_covering(pixels: int) -> int:
    """How many tiles it takes to cover `pixels`, the last one hanging over."""
    return (pixels + WORKGROUP_TILE_SIZE - 1) // WORKGROUP_TILE_SIZE


@dataclass
class ShaderEffectConfig:
    """The look: a Slang `float4 effect(float4 source, int2 at)` function."""

    effect_slang: str = "float4 effect(float4 source, int2 at) { return source; }"


@node.meta(description="Runs the Slang effect function its config carries over every frame")
@node.process
class ShaderEffect:
    """Frame in, the same frame through a Slang `effect` function out."""

    video_from_upstream = Input(Image, delivery="newest", description="Frames to put the look on")
    video_to_downstream = Output(Image, description="The same frames through the look")

    def __init__(self, config: ShaderEffectConfig) -> None:
        self.effect_slang = config.effect_slang

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Depth 1: `dispatch` returns with the GPU work retired, and nothing
        # outside this node names the landing texture.
        self.source_frame_landing_ring = NodeOutputTextureRing(
            TEXTURE_FORMAT, SAMPLED_TEXTURE_USAGE, depth=1
        )
        self.look_frame_ring = NodeOutputTextureRing(
            TEXTURE_FORMAT, STORAGE_AND_SAMPLED_TEXTURE_USAGE
        )
        try:
            self.look_kernel = ctx.gpu_full_access.create_compute_kernel(
                source=look_kernel_slang(self.effect_slang),
                push_constant_size=0,
                bindings={
                    SOURCE_FRAME_BINDING: "sampled_texture",
                    LOOK_FRAME_BINDING: "storage_image",
                },
            )
        except (RuntimeError, ValueError) as refusal:
            raise RuntimeError(
                f"ShaderEffect could not build its look from `effect_slang`: {refusal}"
            ) from refusal

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(self.video_from_upstream)
        if frame is None:
            return

        # A camera publishes buffer-backed frames and a kernel samples
        # textures, so each frame lands in one this node owns, device to device.
        source_frame_landing_texture = (
            self.source_frame_landing_ring.next_texture_for_this_frame(
                ctx.gpu_limited_access, frame.width, frame.height
            )
        )
        ctx.gpu_limited_access.copy_surface_to_surface(
            frame.surface_id, source_frame_landing_texture
        )

        look_frame_texture = self.look_frame_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        self.look_kernel.dispatch(
            bindings={
                SOURCE_FRAME_BINDING: source_frame_landing_texture,
                LOOK_FRAME_BINDING: look_frame_texture,
            },
            group_count=(
                _workgroups_covering(frame.width),
                _workgroups_covering(frame.height),
                1,
            ),
        )

        ctx.outputs.write(
            self.video_to_downstream,
            ImageFrameToWrite.of_texture(look_frame_texture, frame.colour),
            timestamp_ns=frame.timestamp_ns,
        )
