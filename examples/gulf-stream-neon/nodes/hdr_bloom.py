# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The bloom of a game engine: an HDR mip pyramid down and back up, and an anamorphic streak.

Importable as `nodes.hdr_bloom:HdrBloomPyramid`. The way current AAA engines
bloom: a soft-knee threshold with a Karis average against fireflies, a
13-tap downsample through six mips, then a 3x3 tent upsample back up, each
level added onto the one above. Beside it a second chain halves only the width,
for the horizontal streak an anamorphic lens draws off every hot light. Both
come out HDR, for the lens nodes to composite.
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
    node,
)

from nodes.neon_world import HDR_LAYER_FORMAT, RENDERED_FRAME_COLOUR

DOWNSAMPLE_SLANG = """
[[vk::binding(0, 0)]] Sampler2D source;
[[vk::binding(1, 0)]] [format("rgba16f")] RWTexture2D<float4> destination;

struct DownDials {
    float2 destination_size;
    float2 source_texel;
    float threshold;
    float knee;
    float first;
    float horizontal_only;
};
[[vk::push_constant]] ConstantBuffer<DownDials> dials;

float luma(float3 c) { return dot(c, float3(0.2126, 0.7152, 0.0722)); }

float3 prefilter(float3 c) {
    float brightness = max(c.r, max(c.g, c.b));
    float soft = clamp(brightness - dials.threshold + dials.knee, 0.0, 2.0 * dials.knee);
    soft = soft * soft / (4.0 * dials.knee + 1e-4);
    float contribution = max(soft, brightness - dials.threshold) / max(brightness, 1e-4);
    return c * contribution;
}

float3 tap(float2 uv) {
    float3 c = min(source.SampleLevel(uv, 0.0).rgb, float3(64.0, 64.0, 64.0));
    return dials.first > 0.5 ? prefilter(c) : c;
}

float karis(float3 c) { return 1.0 / (1.0 + luma(c)); }

[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (any(float2(id.xy) >= dials.destination_size)) { return; }
    float2 uv = (float2(id.xy) + 0.5) / dials.destination_size;
    float2 t = dials.source_texel;
    float3 result;
    if (dials.horizontal_only > 0.5) {
        // Halve the width only: a wide 6-tap row, so the streak stays one pixel tall.
        result = tap(uv + float2(-2.5 * t.x, 0.0)) * 0.1 + tap(uv + float2(-1.5 * t.x, 0.0)) * 0.15
               + tap(uv + float2(-0.5 * t.x, 0.0)) * 0.25 + tap(uv + float2(0.5 * t.x, 0.0)) * 0.25
               + tap(uv + float2(1.5 * t.x, 0.0)) * 0.15 + tap(uv + float2(2.5 * t.x, 0.0)) * 0.1;
    } else {
        // The 13-tap downsample: five overlapping 2x2 boxes.
        float3 a = tap(uv + t * float2(-2, -2));
        float3 b = tap(uv + t * float2(0, -2));
        float3 c = tap(uv + t * float2(2, -2));
        float3 d = tap(uv + t * float2(-1, -1));
        float3 e = tap(uv + t * float2(1, -1));
        float3 f = tap(uv + t * float2(-2, 0));
        float3 g = tap(uv);
        float3 h = tap(uv + t * float2(2, 0));
        float3 i = tap(uv + t * float2(-1, 1));
        float3 j = tap(uv + t * float2(1, 1));
        float3 k = tap(uv + t * float2(-2, 2));
        float3 l = tap(uv + t * float2(0, 2));
        float3 m = tap(uv + t * float2(2, 2));
        float3 box0 = (d + e + i + j) * 0.25;
        float3 box1 = (a + b + f + g) * 0.25;
        float3 box2 = (b + c + g + h) * 0.25;
        float3 box3 = (f + g + k + l) * 0.25;
        float3 box4 = (g + h + l + m) * 0.25;
        if (dials.first > 0.5) {
            // Karis average on the first level: one firefly cannot flash the whole bloom.
            float w0 = karis(box0) * 0.5;
            float w1 = karis(box1) * 0.125;
            float w2 = karis(box2) * 0.125;
            float w3 = karis(box3) * 0.125;
            float w4 = karis(box4) * 0.125;
            result = (box0 * w0 + box1 * w1 + box2 * w2 + box3 * w3 + box4 * w4) / (w0 + w1 + w2 + w3 + w4);
        } else {
            result = box0 * 0.5 + (box1 + box2 + box3 + box4) * 0.125;
        }
    }
    destination[id.xy] = float4(result, 1.0);
}
"""

UPSAMPLE_SLANG = """
[[vk::binding(0, 0)]] Sampler2D lower;
[[vk::binding(1, 0)]] [format("rgba16f")] RWTexture2D<float4> destination;

struct UpDials {
    float2 destination_size;
    float2 lower_texel;
    float radius;
    float keep;
    float horizontal_only;
    float pad;
};
[[vk::push_constant]] ConstantBuffer<UpDials> dials;

[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (any(float2(id.xy) >= dials.destination_size)) { return; }
    float2 uv = (float2(id.xy) + 0.5) / dials.destination_size;
    float2 t = dials.lower_texel * dials.radius;
    float3 sum;
    if (dials.horizontal_only > 0.5) {
        sum = lower.SampleLevel(uv + float2(-t.x, 0.0), 0.0).rgb * 0.25
            + lower.SampleLevel(uv, 0.0).rgb * 0.5
            + lower.SampleLevel(uv + float2(t.x, 0.0), 0.0).rgb * 0.25;
    } else {
        // The 3x3 tent.
        sum = lower.SampleLevel(uv + t * float2(-1, -1), 0.0).rgb
            + lower.SampleLevel(uv + t * float2(0, -1), 0.0).rgb * 2.0
            + lower.SampleLevel(uv + t * float2(1, -1), 0.0).rgb
            + lower.SampleLevel(uv + t * float2(-1, 0), 0.0).rgb * 2.0
            + lower.SampleLevel(uv, 0.0).rgb * 4.0
            + lower.SampleLevel(uv + t * float2(1, 0), 0.0).rgb * 2.0
            + lower.SampleLevel(uv + t * float2(-1, 1), 0.0).rgb
            + lower.SampleLevel(uv + t * float2(0, 1), 0.0).rgb * 2.0
            + lower.SampleLevel(uv + t * float2(1, 1), 0.0).rgb;
        sum /= 16.0;
    }
    float3 here = destination[id.xy].rgb;
    destination[id.xy] = float4(here * dials.keep + sum, 1.0);
}
"""

DOWN_DIALS = struct.Struct("<8f")
UP_DIALS = struct.Struct("<8f")
LAYER_USAGE = ["storage_binding", "texture_binding"]
BLOOM_LEVELS = 6
STREAK_LEVELS = 6


def groups(width: int, height: int) -> tuple[int, int, int]:
    return ((width + 7) // 8, (height + 7) // 8, 1)


@node.meta(
    category="Gulf Stream Neon",
    description="AAA HDR bloom: soft-knee threshold, Karis average, a 13-tap downsample through six mips and a tent upsample back, plus an anamorphic streak chain.",
)
@node
class HdrBloomPyramid:
    """The bloom pyramid and the anamorphic streak of an HDR picture."""

    threshold: Annotated[
        float, NumberRange(minimum=0.0), Description("The brightness the glow starts at.")
    ] = 1.6
    knee: Annotated[
        float, NumberRange(minimum=0.0), Description("How softly the glow fades in below the threshold.")
    ] = 0.8
    radius: Annotated[
        float, NumberRange(minimum=0.0), Description("How wide the bloom spreads.")
    ] = 1.0
    streak_radius: Annotated[
        float, NumberRange(minimum=0.0), Description("How wide the anamorphic streak spreads.")
    ] = 1.6
    scene: InputPin[Annotated[Texture, StreamDelivery("newest"), Description("The HDR picture")]]
    bloom: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(HDR_LAYER_FORMAT),
            Description("The bloom, HDR, at half the picture's size"),
        ]
    ]
    streak: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(HDR_LAYER_FORMAT),
            Description("The anamorphic streak, HDR, at half the picture's size"),
        ]
    ]

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        gpu = ctx.gpu_full_access
        self.down = gpu.create_compute_kernel(source=DOWNSAMPLE_SLANG, push_constant_size=DOWN_DIALS.size)
        self.up = gpu.create_compute_kernel(source=UPSAMPLE_SLANG, push_constant_size=UP_DIALS.size)
        self.bloom_ring = NodeOutputTextureRing(HDR_LAYER_FORMAT, LAYER_USAGE)
        self.streak_ring = NodeOutputTextureRing(HDR_LAYER_FORMAT, LAYER_USAGE)
        self.extent: tuple[int, int] | None = None
        self.gpu_full = gpu

    def _allocate(self, gpu, width: int, height: int) -> None:
        self.extent = (width, height)
        self.mips = []
        w, h = width, height
        for _ in range(BLOOM_LEVELS):
            w, h = max(1, w // 2), max(1, h // 2)
            self.mips.append((gpu.acquire_texture(w, h, HDR_LAYER_FORMAT, LAYER_USAGE), w, h))
        self.streaks = []
        w, h = width // 2, height // 4
        self.streaks.append((gpu.acquire_texture(w, h, HDR_LAYER_FORMAT, LAYER_USAGE), w, h))
        for _ in range(STREAK_LEVELS - 1):
            w = max(1, w // 2)
            self.streaks.append((gpu.acquire_texture(w, h, HDR_LAYER_FORMAT, LAYER_USAGE), w, h))

    def _down(self, source, source_width, source_height, destination, width, height, first, horizontal, threshold):
        self.down.dispatch(
            bindings={"source": source, "destination": destination},
            group_count=groups(width, height),
            push_constants=DOWN_DIALS.pack(
                width, height, 1.0 / source_width, 1.0 / source_height,
                threshold, self.knee, 1.0 if first else 0.0, 1.0 if horizontal else 0.0,
            ),
        )

    def _up(self, lower, lower_width, lower_height, destination, width, height, radius, keep, horizontal):
        self.up.dispatch(
            bindings={"lower": lower, "destination": destination},
            group_count=groups(width, height),
            push_constants=UP_DIALS.pack(
                width, height, 1.0 / lower_width, 1.0 / lower_height, radius, keep, 1.0 if horizontal else 0.0, 0.0,
            ),
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        scene = self.scene.read()
        if scene is None:
            return
        width, height = scene.width, scene.height
        if self.extent != (width, height):
            self._allocate(self.gpu_full, width, height)
        threshold = self.threshold
        gpu = ctx.gpu_limited_access

        source, sw, sh = scene.surface_id, width, height
        for level, (texture, w, h) in enumerate(self.mips):
            self._down(source, sw, sh, texture, w, h, level == 0, False, threshold)
            source, sw, sh = texture, w, h
        for level in range(BLOOM_LEVELS - 1, 0, -1):
            lower, lw, lh = self.mips[level]
            upper, uw, uh = self.mips[level - 1]
            self._up(lower, lw, lh, upper, uw, uh, self.radius, 1.0, False)

        source, sw, sh = scene.surface_id, width, height
        for level, (texture, w, h) in enumerate(self.streaks):
            self._down(source, sw, sh, texture, w, h, level == 0, level > 0, threshold * 1.4)
            source, sw, sh = texture, w, h
        for level in range(STREAK_LEVELS - 1, 0, -1):
            lower, lw, lh = self.streaks[level]
            upper, uw, uh = self.streaks[level - 1]
            self._up(lower, lw, lh, upper, uw, uh, self.streak_radius, 1.0, True)

        top, tw, th = self.mips[0]
        bloom_out = self.bloom_ring.next_texture_for_this_frame(gpu, tw, th)
        gpu.copy_surface_to_surface(top.surface_id, bloom_out)
        streak_top, stw, sth = self.streaks[0]
        streak_out = self.streak_ring.next_texture_for_this_frame(gpu, stw, sth)
        gpu.copy_surface_to_surface(streak_top.surface_id, streak_out)
        self.bloom.write(ImageFrameToWrite.of_texture(bloom_out, RENDERED_FRAME_COLOUR), timestamp_ns=scene.timestamp_ns)
        self.streak.write(ImageFrameToWrite.of_texture(streak_out, RENDERED_FRAME_COLOUR), timestamp_ns=scene.timestamp_ns)
