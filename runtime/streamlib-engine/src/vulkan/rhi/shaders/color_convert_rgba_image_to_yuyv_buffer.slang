// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only
//
// RGBA sampled-texture source → YUYV buffer dest color converter, the
// reverse of `color_convert_yuyv_buffer_to_rgba.slang`.
//
// YUYV (a.k.a. YUY2): 4:2:2 subsampling with `[Y0, U, Y1, V]` macro-
// pixels packed into uint32. One thread writes one macropixel — two
// horizontally-adjacent pixels sharing the average of their (Cb, Cr).
// Honors destination-side row stride padding via `plane0_stride_bytes`,
// which is the V4L2 `bytesperline` and must be a multiple of 4;
// `plane1_stride_bytes` and `plane1_offset_bytes` are unused.
//
// Bindings:
//   0 — `rgba_input`: a combined image sampler, fetched by integer texel,
//       never filtered.
//   1 — `yuyv_output`: one uint32 per macropixel.

import color_convert_common;

[[vk::binding(0, 0)]] Sampler2D rgba_input;
[[vk::binding(1, 0)]] RWStructuredBuffer<uint> yuyv_output;

[[vk::push_constant]] ConstantBuffer<ColorConverterPushConstants> pc;

float3 ycbcr_bytes_at(uint x, uint y) {
    return convert_color_to_ycbcr_bytes(rgba_input.Load(int3(int(x), int(y), 0)).rgb, pc);
}

[numthreads(16, 16, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint2 macro_pos = invocation.xy;
    uint x0 = macro_pos.x * 2u;
    if (x0 >= pc.width || macro_pos.y >= pc.height) return;
    // An odd width's last macropixel repeats its only pixel.
    uint x1 = min(x0 + 1u, pc.width - 1u);

    float3 left = ycbcr_bytes_at(x0, macro_pos.y);
    float3 right = ycbcr_bytes_at(x1, macro_pos.y);

    uint y0 = uint(round(left.x));
    uint y1 = uint(round(right.x));
    uint cb = uint(round(0.5 * (left.y + right.y)));
    uint cr = uint(round(0.5 * (left.z + right.z)));

    uint macro_pixel = (y0 & 0xFFu)
        | ((cb & 0xFFu) << 8u)
        | ((y1 & 0xFFu) << 16u)
        | ((cr & 0xFFu) << 24u);

    uint macro_uint_stride = pc.plane0_stride_bytes >> 2u;
    yuyv_output[macro_pos.y * macro_uint_stride + macro_pos.x] = macro_pixel;
}
