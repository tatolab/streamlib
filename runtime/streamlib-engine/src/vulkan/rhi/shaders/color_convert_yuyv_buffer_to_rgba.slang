// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only
//
// YUYV buffer-source → RGBA storage-image-dest color converter.
//
// YUYV (a.k.a. YUY2): 4:2:2 subsampling with `[Y0, U, Y1, V]` macro-
// pixels packed into uint32. Two horizontally-adjacent pixels share
// one (U, V) pair. Honors source-side row stride padding via
// `plane0_stride_bytes` — must be a multiple of 4 (uint32 align);
// `plane1_stride_bytes` and `plane1_offset_bytes` are unused.
//
// Bindings:
//   0 — `yuyv_input`: one uint32 per macropixel.
//   1 — `rgba_output`: an RGBA8 storage image.

import color_convert_common;

[[vk::binding(0, 0)]] StructuredBuffer<uint> yuyv_input;
[[vk::binding(1, 0)]] [format("rgba8")] WTexture2D<float4> rgba_output;

[[vk::push_constant]] ConstantBuffer<ColorConverterPushConstants> pc;

[numthreads(16, 16, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint2 pos = invocation.xy;
    if (pos.x >= pc.width || pos.y >= pc.height) return;

    // Each uint32 packs two adjacent pixels [Y0, U, Y1, V].
    // Row stride in uint32: plane0_stride_bytes >> 2.
    uint macro_uint_stride = pc.plane0_stride_bytes >> 2u;
    uint macro_index = pos.y * macro_uint_stride + (pos.x >> 1u);
    uint macro_pixel = yuyv_input[macro_index];

    float Y0 = float(macro_pixel & 0xFFu);
    float U  = float((macro_pixel >> 8u) & 0xFFu);
    float Y1 = float((macro_pixel >> 16u) & 0xFFu);
    float V  = float((macro_pixel >> 24u) & 0xFFu);

    // Select Y0 for even pixels, Y1 for odd pixels; U/V shared.
    float Y = ((pos.x & 1u) == 0u) ? Y0 : Y1;

    rgba_output.Store(pos, float4(convert_color(float3(Y, U, V), pc), 1.0));
}
