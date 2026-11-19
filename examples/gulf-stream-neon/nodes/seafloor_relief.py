# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The seafloor as neon contour lines, drawn into a world-space texture.

Importable as `nodes.seafloor_relief:SeafloorRelief`. The relief is built once
by one compute kernel into a held texture; a second kernel draws its contour
lines every frame, coloured by depth and pulsing on the soundtrack's beat.
"""

from __future__ import annotations

import struct

from tatolab.stream import (
    Image,
    ImageFrameToWrite,
    Input,
    NodeOutputTextureRing,
    Output,
    Records,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    node,
)

from nodes.neon_world import (
    TakeClock,
    NANOSECONDS_PER_SECOND,
    RENDERED_FRAME_COLOUR,
    WORLD_SLANG,
    WORLD_TEXTURE_SIZE,
)

LAYER_FORMAT = "rgba16_float"

RELIEF_SLANG = WORLD_SLANG + """
[[vk::binding(0, 0)]] [format("rgba16f")] RWTexture2D<float4> relief;

struct ReliefDials { float texture_size; };
[[vk::push_constant]] ConstantBuffer<ReliefDials> dials;

// r: elevation in km (negative below sea level), g: land, b: city density.
[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (any(float2(id.xy) >= dials.texture_size)) { return; }
    float2 w = uv_to_world((float2(id.xy) + 0.5) / dials.texture_size);
    float coast = coast_x(w.y) + 0.04 * (fbm(w * 6.0, 4) - 0.5);
    float island = barrier_island_x(w.y);
    float edge = shelf_edge_x(w.y) + 0.05 * (fbm(w * 3.0 + 7.0, 4) - 0.5);
    float detail = fbm(w * 9.0 + 3.0, 5) - 0.5;

    float elevation;
    float land = 0.0;
    float city = 0.0;
    if (w.x < coast) {
        land = 1.0;
        elevation = 0.004 + 0.006 * fbm(w * 4.0, 3);
        float inland = coast - w.x;
        city = smoothstep(0.75, 0.05, inland) * smoothstep(0.35, 0.6, fbm(w * 7.0 + 11.0, 4));
    } else if (abs(w.x - island) < 0.022 && fbm(float2(0.0, w.y * 5.0), 3) > 0.32) {
        land = 1.0;
        elevation = 0.003;
        city = 1.0;
    } else if (w.x < island) {
        elevation = -0.003 - 0.002 * fbm(w * 12.0, 3);
    } else if (w.x < edge) {
        float across = saturate((w.x - island) / max(edge - island, 0.05));
        elevation = -(0.005 + 0.075 * across * across) * (1.0 + 0.25 * detail);
    } else {
        float beyond = w.x - edge;
        float canyon = 0.12 * smoothstep(0.55, 0.8, fbm(float2(w.y * 4.0, beyond * 2.0), 4));
        elevation = -0.08 - 0.72 * smoothstep(0.0, 0.32, beyond) - 0.12 * beyond
                    - canyon + 0.06 * detail;
    }
    relief[id.xy] = float4(elevation, land, city, 1.0);
}
"""

CONTOUR_SLANG = WORLD_SLANG + """
[[vk::binding(0, 0)]] [format("rgba16f")] RWTexture2D<float4> relief;
[[vk::binding(1, 0)]] [format("rgba16f")] RWTexture2D<float4> contour_layer;

struct ContourDials {
    float texture_size;
    float seconds;
    float glow;
    float beat;
};
[[vk::push_constant]] ConstantBuffer<ContourDials> dials;

float3 depth_colour(float depth_fraction) {
    float3 c = lerp(PEACH, CORAL, smoothstep(0.0, 0.15, depth_fraction));
    c = lerp(c, HOT_PINK, smoothstep(0.12, 0.35, depth_fraction));
    c = lerp(c, MAGENTA, smoothstep(0.35, 0.65, depth_fraction));
    return lerp(c, CYAN, smoothstep(0.62, 0.95, depth_fraction));
}

float contour_value(float elevation_km) {
    // Square-root depth packs lines on the shelf and spreads them in the deep.
    return sqrt(max(-elevation_km, 0.0) * 1000.0) / 1.15;
}

float4 relief_at(int2 p) {
    int size = int(dials.texture_size);
    return relief[clamp(p, int2(0, 0), int2(size - 1, size - 1))];
}

[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (any(float2(id.xy) >= dials.texture_size)) { return; }
    int2 p = int2(id.xy);
    float4 here = relief_at(p);
    float2 w = uv_to_world((float2(id.xy) + 0.5) / dials.texture_size);
    float3 colour = float3(0.0, 0.0, 0.0);

    float land_around = relief_at(p + int2(2, 0)).g + relief_at(p - int2(2, 0)).g
                      + relief_at(p + int2(0, 2)).g + relief_at(p - int2(0, 2)).g;
    float shoreline = (here.g > 0.5) ? (4.0 - land_around) / 4.0 : land_around / 4.0;

    if (here.g > 0.5) {
        // Land: a dark violet with the city's street grid glowing warm.
        colour = NIGHT * 0.55;
        float2 street = abs(frac(w * float2(38.0, 52.0)) - 0.5);
        float street_line = saturate(1.0 - min(street.x, street.y) * 14.0);
        float twinkle = 0.6 + 0.4 * sin(dials.seconds * 3.0 + hash21(floor(w * 52.0)) * 40.0);
        float lit = here.b * street_line * twinkle;
        float3 street_colour = lerp(PEACH, CORAL, hash21(floor(w * 19.0)));
        colour += street_colour * lit * 0.9;
        float block_light = step(0.93, hash21(floor(w * 90.0))) * here.b;
        colour += lerp(HOT_PINK, CYAN, hash21(floor(w * 90.0) + 3.0)) * block_light * 1.4;
    } else {
        float v = contour_value(here.r);
        float vx = contour_value(relief_at(p + int2(1, 0)).r) - contour_value(relief_at(p - int2(1, 0)).r);
        float vy = contour_value(relief_at(p + int2(0, 1)).r) - contour_value(relief_at(p - int2(0, 1)).r);
        float per_texel = max(length(float2(vx, vy)) * 0.5, 1e-4);
        float distance_to_line = abs(frac(v + 0.5) - 0.5) / per_texel;
        float line = saturate(1.3 - distance_to_line * 0.75);
        float major = (fmod(floor(v + 0.5), 5.0) < 0.5) ? 1.8 : 1.0;

        float depth_fraction = saturate(v / 26.0);
        float3 line_colour = depth_colour(depth_fraction);
        // A wave of light rolls outward from the shelf edge on every beat.
        float wave = exp(-pow(frac(v * 0.08 - dials.seconds * 0.35) - 0.5, 2.0) * 90.0);
        float pulse = 1.0 + 1.6 * wave * (0.35 + dials.beat);
        colour = line_colour * line * major * 1.3 * pulse * dials.glow;
        // The faintest wash of the depth colour, so the deep is not flat black.
        colour += line_colour * 0.035 + NIGHT * 0.25;
    }
    colour += HOT_PINK * shoreline * 1.6 * dials.glow;
    contour_layer[id.xy] = float4(colour, 1.0);
}
"""

RELIEF_DIALS = struct.Struct("<f")
CONTOUR_DIALS = struct.Struct("<ffff")
WORKGROUP = 8


def workgroups(pixels: int) -> int:
    return (pixels + WORKGROUP - 1) // WORKGROUP


@node.process
class SeafloorRelief:
    """Neon contour lines of a shelf that drops into the deep, pulsing on the beat."""

    soundtrack = Input(Records, delivery="newest", description="From SoundtrackPulse")
    contours_to_downstream = Output(
        Image, description="The contour layer in world space, HDR"
    )

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.take_clock = TakeClock()
        gpu = ctx.gpu_full_access
        self.relief_kernel = gpu.create_compute_kernel(
            source=RELIEF_SLANG, push_constant_size=RELIEF_DIALS.size
        )
        self.contour_kernel = gpu.create_compute_kernel(
            source=CONTOUR_SLANG, push_constant_size=CONTOUR_DIALS.size
        )
        self.relief = gpu.acquire_texture(
            WORLD_TEXTURE_SIZE,
            WORLD_TEXTURE_SIZE,
            LAYER_FORMAT,
            ["storage_binding", "texture_binding"],
        )
        self.relief_kernel.dispatch(
            bindings={"relief": self.relief},
            group_count=(workgroups(WORLD_TEXTURE_SIZE), workgroups(WORLD_TEXTURE_SIZE), 1),
            push_constants=RELIEF_DIALS.pack(WORLD_TEXTURE_SIZE),
        )
        self.contour_textures = NodeOutputTextureRing(
            LAYER_FORMAT, ["storage_binding", "texture_binding"]
        )
        self.latest_soundtrack: dict = {}

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        soundtrack = ctx.inputs.read(self.soundtrack)
        if soundtrack is not None:
            self.latest_soundtrack = soundtrack
        seconds = self.take_clock.seconds_at(ctx)
        beat = float(self.latest_soundtrack.get("beat", 0.0))
        glow = float(self.latest_soundtrack.get("glow", 1.0))
        texture = self.contour_textures.next_texture_for_this_frame(
            ctx.gpu_limited_access, WORLD_TEXTURE_SIZE, WORLD_TEXTURE_SIZE
        )
        self.contour_kernel.dispatch(
            bindings={"relief": self.relief, "contour_layer": texture},
            group_count=(workgroups(WORLD_TEXTURE_SIZE), workgroups(WORLD_TEXTURE_SIZE), 1),
            push_constants=CONTOUR_DIALS.pack(WORLD_TEXTURE_SIZE, seconds, glow, beat),
        )
        ctx.outputs.write(
            self.contours_to_downstream,
            ImageFrameToWrite.of_texture(texture, RENDERED_FRAME_COLOUR),
            timestamp_ns=ctx.time,
        )
