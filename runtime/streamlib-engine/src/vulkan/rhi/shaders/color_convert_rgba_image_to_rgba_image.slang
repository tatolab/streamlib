// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only
//
// Sampled RGBA or BGRA texture source → RGBA storage-image dest, texel for
// texel. A sampled BGRA image reads back in RGBA order, so the swizzle is
// the sampler's.
//
// Bindings:
//   0 — `color_input`: a combined image sampler, fetched by integer texel,
//       never filtered.
//   1 — `rgba_output`: an RGBA8 storage image.

[[vk::binding(0, 0)]] Sampler2D color_input;
[[vk::binding(1, 0)]] [format("rgba8")] WTexture2D<float4> rgba_output;

[numthreads(16, 16, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint2 extent;
    rgba_output.GetDimensions(extent.x, extent.y);
    uint2 pixel = invocation.xy;
    if (pixel.x >= extent.x || pixel.y >= extent.y) return;
    rgba_output.Store(pixel, color_input.Load(int3(int2(pixel), 0)));
}
