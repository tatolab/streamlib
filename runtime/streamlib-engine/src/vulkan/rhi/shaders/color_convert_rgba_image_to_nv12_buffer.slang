// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// RGBA sampled-texture source → NV12 buffer dest color converter, the
// reverse of `color_convert_nv12_buffer_to_rgba.slang`.
//
// One thread writes a 4×2 pixel block: one uint of four luma bytes on each
// of its two rows, and one uint of two (Cb, Cr) pairs on its chroma row, each
// pair the box average of its 2×2 pixels. Pixels past the width or height
// repeat the last column or row, so every write lands inside a stride that
// holds the width rounded up to 4. `plane0_stride_bytes`,
// `plane1_stride_bytes` and `plane1_offset_bytes` are each a multiple of 4.
//
// Bindings:
//   0 — `rgba_input`: a combined image sampler, fetched by integer texel,
//       never filtered.
//   1 — `nv12_output`: uint-addressed; plane 0 at byte 0.

import color_convert_common;

[[vk::binding(0, 0)]] Sampler2D rgba_input;
[[vk::binding(1, 0)]] RWStructuredBuffer<uint> nv12_output;

[[vk::push_constant]] ConstantBuffer<ColorConverterPushConstants> pc;

float3 ycbcr_bytes_at(uint x, uint y) {
    uint clamped_x = min(x, pc.width - 1u);
    uint clamped_y = min(y, pc.height - 1u);
    return convert_color_to_ycbcr_bytes(
        rgba_input.Load(int3(int(clamped_x), int(clamped_y), 0)).rgb, pc);
}

uint byte_of(float value) {
    return uint(round(value)) & 0xFFu;
}

[numthreads(16, 16, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint x0 = invocation.x * 4u;
    uint y0 = invocation.y * 2u;
    if (x0 >= pc.width || y0 >= pc.height) return;

    float3 top[4];
    float3 bottom[4];
    for (uint column = 0u; column < 4u; column++) {
        top[column] = ycbcr_bytes_at(x0 + column, y0);
        bottom[column] = ycbcr_bytes_at(x0 + column, y0 + 1u);
    }

    uint top_luma = byte_of(top[0].x)
        | (byte_of(top[1].x) << 8u)
        | (byte_of(top[2].x) << 16u)
        | (byte_of(top[3].x) << 24u);
    nv12_output[(y0 * pc.plane0_stride_bytes + x0) >> 2u] = top_luma;
    if (y0 + 1u < pc.height) {
        uint bottom_luma = byte_of(bottom[0].x)
            | (byte_of(bottom[1].x) << 8u)
            | (byte_of(bottom[2].x) << 16u)
            | (byte_of(bottom[3].x) << 24u);
        nv12_output[((y0 + 1u) * pc.plane0_stride_bytes + x0) >> 2u] = bottom_luma;
    }

    float2 left_chroma = 0.25 * (top[0].yz + top[1].yz + bottom[0].yz + bottom[1].yz);
    float2 right_chroma = 0.25 * (top[2].yz + top[3].yz + bottom[2].yz + bottom[3].yz);
    uint chroma = byte_of(left_chroma.x)
        | (byte_of(left_chroma.y) << 8u)
        | (byte_of(right_chroma.x) << 16u)
        | (byte_of(right_chroma.y) << 24u);
    uint chroma_row = y0 >> 1u;
    nv12_output[(pc.plane1_offset_bytes + chroma_row * pc.plane1_stride_bytes + x0) >> 2u] =
        chroma;
}
