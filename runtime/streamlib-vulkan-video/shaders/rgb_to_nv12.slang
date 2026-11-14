// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only
//
// RGB(A) to NV12 compute shader.
// Based on pyroenc rgb_to_yuv.comp by Arntzen Software AS (MIT license).
//
// Input:  sampled texture2D (RGBA or BGRA — a texel load handles any format)
// Output: per-plane storage images — R8 for luma, RG8 for chroma
//
// Each thread converts a 2x2 luma block + 1 chroma sample.
// 6-tap left-sited chroma filter for MPEG-2/4 chroma siting.
// BT.709 color matrix with TV range (16-235 luma, 16-240 chroma).

[[vk::binding(0, 0)]] Texture2D<float4> uImage;
[[vk::binding(1, 0)]] [format("r8")] WTexture2D<float> uLuma;
[[vk::binding(2, 0)]] [format("rg8")] WTexture2D<float2> uChroma;

struct RgbToNv12Registers {
    int2 resolution;
};
[[vk::push_constant]] ConstantBuffer<RgbToNv12Registers> registers;

// BT.709 RGB-to-YCbCr matrix rows.
static const float3 RGB_TO_LUMA = float3(0.2126, 0.7152, 0.0722);
static const float3 RGB_TO_CB = float3(-0.114572, -0.385428, 0.5);
static const float3 RGB_TO_CR = float3(0.5, -0.454153, -0.0458471);

float3 rgb_at(int x, int y) {
    return uImage.Load(int3(x, y, 0)).rgb;
}

[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID)
{
    // Each thread works on a 2x2 luma region and 1x1 chroma region.
    int2 coord = int2(invocation.xy) * 2;

    // Left-sited chroma filtering: 6 taps (3x2 texels).
    int x0 = clamp(coord.x - 1, 0, registers.resolution.x - 1);
    int x1 = min(coord.x, registers.resolution.x - 1);
    int x2 = min(coord.x + 1, registers.resolution.x - 1);

    int y0 = min(coord.y, registers.resolution.y - 1);
    int y1 = min(coord.y + 1, registers.resolution.y - 1);

    float3 rgb00 = rgb_at(x0, y0);
    float3 rgb10 = rgb_at(x1, y0);
    float3 rgb20 = rgb_at(x2, y0);
    float3 rgb01 = rgb_at(x0, y1);
    float3 rgb11 = rgb_at(x1, y1);
    float3 rgb21 = rgb_at(x2, y1);

    // Compute luma for each pixel in the 2x2 block.
    float4 luma = float4(
        dot(rgb10, RGB_TO_LUMA),
        dot(rgb20, RGB_TO_LUMA),
        dot(rgb11, RGB_TO_LUMA),
        dot(rgb21, RGB_TO_LUMA));

    // Scale to TV range (16-235).
    luma = luma * (219.0 / 255.0) + (16.0 / 255.0);

    uint2 luma_coord = uint2(coord);
    uLuma.Store(luma_coord, luma.x);
    uLuma.Store(luma_coord + uint2(1, 0), luma.y);
    uLuma.Store(luma_coord + uint2(0, 1), luma.z);
    uLuma.Store(luma_coord + uint2(1, 1), luma.w);

    // Chroma: filter in RGB domain (linear transform is commutative with
    // linear filtering), then convert to CbCr.
    // 6-tap weights: center pair 0.25 each, neighbours 0.125 each.
    float3 rgb = rgb10 * 0.25 + rgb11 * 0.25 +
        rgb00 * 0.125 + rgb01 * 0.125 +
        rgb20 * 0.125 + rgb21 * 0.125;

    float2 cbcr = float2(dot(rgb, RGB_TO_CB), dot(rgb, RGB_TO_CR));
    // Scale to TV range (16-240).
    cbcr = cbcr * (224.0 / 255.0) + (128.0 / 255.0);

    uChroma.Store(invocation.xy, cbcr);
}
