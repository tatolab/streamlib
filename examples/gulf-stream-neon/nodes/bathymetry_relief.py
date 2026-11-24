# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The real seafloor off Miami, drawn as neon depth contours in a world-space texture.

Importable as `nodes.bathymetry_relief:BathymetryRelief`. `setup()` uploads
NOAA's ETOPO 2022 elevation for the map's square once and a kernel bakes it
into a held texture; each frame a second kernel draws its isobaths, coloured
by depth and pulsing outward on the soundtrack's beat. It sends both: the
contours to light the sea, and the relief itself, so the renderer can raise a
skyline on the real coast and draw the real chart at the reveal.
"""

from __future__ import annotations

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
    log,
    node,
)

from nodes.neon_world import (
    HDR_LAYER_FORMAT,
    RENDERED_FRAME_COLOUR,
    WORLD_SLANG,
    WORLD_TEXTURE_SIZE,
    SoundtrackPulseReading,
    TakeClock,
)
from nodes.ocean_data import baked_relief

CONTOUR_SLANG = WORLD_SLANG + """
[[vk::binding(0, 0)]] [format("rgba32f")] RWTexture2D<float4> relief;
[[vk::binding(1, 0)]] [format("rgba16f")] RWTexture2D<float4> contour_layer;

struct ContourDials {
    float texture_size;
    float seconds;
    float glow;
    float beat;
    float line_spacing_m;
};
[[vk::push_constant]] ConstantBuffer<ContourDials> dials;

float3 depth_colour(float depth_fraction) {
    float3 c = lerp(TATO_CREAM, TATO_YELLOW, smoothstep(0.0, 0.08, depth_fraction));
    c = lerp(c, TATO_PINK, smoothstep(0.06, 0.25, depth_fraction));
    c = lerp(c, TATO_VIOLET, smoothstep(0.25, 0.55, depth_fraction));
    return lerp(c, TATO_BLUE, smoothstep(0.55, 0.95, depth_fraction));
}

// Square-root depth packs the isobaths on the shelf and spreads them in the deep.
float contour_value(float elevation_km) {
    return sqrt(max(-elevation_km, 0.0) * 1000.0 / dials.line_spacing_m);
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

    // The real coastline: within a texel or two of where land meets water.
    float shoreline = exp(-here.a * here.a / 0.03);

    if (here.g > 0.5) {
        // Land: a dark violet with the cities' street grids glowing warm.
        colour = TATO_INK * 0.6;
        float2 street = abs(frac(w * float2(160.0, 200.0)) - 0.5);
        float street_line = saturate(1.0 - min(street.x, street.y) * 9.0);
        float twinkle = 0.6 + 0.4 * sin(dials.seconds * 3.0 + hash21(floor(w * 200.0)) * 40.0);
        float lit = saturate(here.b * 1.6) * street_line * twinkle;
        float3 street_colour = lerp(TATO_CREAM, TATO_YELLOW, hash21(floor(w * 60.0)));
        colour += street_colour * lit * 1.2;
        // The Everglades and the inland grid: faint, so the coast carries the light.
        colour += TATO_CREAM * 0.05 * street_line * smoothstep(0.0, 30.0, here.a) * step(0.6, hash21(floor(w * 40.0)));
    } else {
        float v = contour_value(here.r);
        float vx = contour_value(relief_at(p + int2(1, 0)).r) - contour_value(relief_at(p - int2(1, 0)).r);
        float vy = contour_value(relief_at(p + int2(0, 1)).r) - contour_value(relief_at(p - int2(0, 1)).r);
        float per_texel = max(length(float2(vx, vy)) * 0.5, 1e-4);
        float distance_to_line = abs(frac(v + 0.5) - 0.5) / per_texel;
        float line = saturate(1.2 - distance_to_line * 0.8);
        float major = (fmod(floor(v + 0.5), 5.0) < 0.5) ? 1.9 : 1.0;

        float depth_fraction = saturate(v / 26.0);
        float3 line_colour = depth_colour(depth_fraction);
        // A wave of light rolls out from the coast into the deep on every beat.
        float wave = exp(-pow(frac(v * 0.06 - dials.seconds * 0.5) - 0.5, 2.0) * 80.0);
        float pulse = 1.0 + 2.0 * wave * (0.25 + dials.beat);
        colour = line_colour * line * major * 0.7 * pulse * dials.glow;
        // The faintest wash of the depth colour, and the shallow banks lit turquoise.
        float shallow = smoothstep(-0.012, -0.001, here.r);
        colour += line_colour * 0.03 + DEEP_BAND * 0.22 + TATO_MINT * shallow * 0.08;
    }
    colour += TATO_PINK * shoreline * 2.2 * dials.glow;
    contour_layer[id.xy] = float4(colour, 1.0);
}
"""

RELIEF_FORMAT = "rgba32_float"
CONTOUR_DIALS = struct.Struct("<fffff")
WORKGROUP = 8
LAYER_USAGE = ["storage_binding", "texture_binding"]


def workgroups(pixels: int) -> int:
    return (pixels + WORKGROUP - 1) // WORKGROUP


@node.meta(
    category="Gulf Stream Neon",
    description="The real seafloor off Miami (NOAA ETOPO 2022) as neon isobaths that pulse on the beat, plus the raw relief.",
)
@node
class BathymetryRelief:
    """Neon isobaths of the real Florida Straits, pulsing on the beat."""

    line_spacing_m: Annotated[
        float, NumberRange(minimum=0.01), Description("The depth between two isobaths, in metres.")
    ] = 1.6
    glow: Annotated[
        float, NumberRange(minimum=0.0), Description("How bright the isobaths glow.")
    ] = 1.0
    soundtrack: InputPin[
        Annotated[
            SoundtrackPulseReading,
            StreamDelivery("newest"),
            Description("beat, energy: from SoundtrackPulse"),
        ]
    ]
    contours: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(HDR_LAYER_FORMAT),
            Description("The isobath layer in world space, HDR"),
        ]
    ]
    relief: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(RELIEF_FORMAT),
            Description("Elevation km, land, city density, km to the coast, in world space"),
        ]
    ]

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.take_clock = TakeClock()
        gpu = ctx.gpu_full_access
        size = WORLD_TEXTURE_SIZE
        self.contour_kernel = gpu.create_compute_kernel(source=CONTOUR_SLANG, push_constant_size=CONTOUR_DIALS.size)
        baked = baked_relief(size)
        # The relief is held at full float precision: elevations of a few metres matter on the banks.
        self.held_relief = gpu.acquire_texture(size, size, RELIEF_FORMAT, LAYER_USAGE)
        self.held_relief.lock(read_only=False)
        self.held_relief.as_numpy().reshape(size * size, 4)[:] = baked
        self.held_relief.unlock()
        self.contour_textures = NodeOutputTextureRing(HDR_LAYER_FORMAT, LAYER_USAGE)
        self.relief_textures = NodeOutputTextureRing(RELIEF_FORMAT, LAYER_USAGE)
        self.latest_soundtrack = SoundtrackPulseReading()
        log.info("bathymetry uploaded", texels=size * size, deepest_km=float(baked[:, 0].min()))

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        soundtrack = self.soundtrack.read()
        if soundtrack is not None:
            self.latest_soundtrack = soundtrack
        seconds = self.take_clock.seconds_at(ctx)
        beat = self.latest_soundtrack.beat
        glow = self.glow * self.latest_soundtrack.glow
        size = WORLD_TEXTURE_SIZE
        gpu = ctx.gpu_limited_access
        contour_texture = self.contour_textures.next_texture_for_this_frame(gpu, size, size)
        self.contour_kernel.dispatch(
            bindings={"relief": self.held_relief, "contour_layer": contour_texture},
            group_count=(workgroups(size), workgroups(size), 1),
            push_constants=CONTOUR_DIALS.pack(size, seconds, glow, beat, self.line_spacing_m),
        )
        relief_texture = self.relief_textures.next_texture_for_this_frame(gpu, size, size)
        gpu.copy_surface_to_surface(self.held_relief.surface_id, relief_texture)
        self.contours.write(
            ImageFrameToWrite.of_texture(contour_texture, RENDERED_FRAME_COLOUR),
            timestamp_ns=ctx.time,
        )
        self.relief.write(
            ImageFrameToWrite.of_texture(relief_texture, RENDERED_FRAME_COLOUR),
            timestamp_ns=ctx.time,
        )
