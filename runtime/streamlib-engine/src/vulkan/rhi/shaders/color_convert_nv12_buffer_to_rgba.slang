// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only
//
// NV12 buffer-source → RGBA storage-image-dest color converter.
//
// Bindings:
//   0 — `nv12_input`: the Y plane then the UV plane, packed in a uint32
//       array, little-endian byte order. Honors source-side row stride
//       padding via `plane0_stride_bytes` / `plane1_stride_bytes` /
//       `plane1_offset_bytes` — vivid + some UVC drivers report
//       stride > width even for the 1-byte-per-pixel NV12 Y plane.
//   1 — `rgba_output`: an RGBA8 storage image.

import color_convert_common;

[[vk::binding(0, 0)]] StructuredBuffer<uint> nv12_input;
[[vk::binding(1, 0)]] [format("rgba8")] WTexture2D<float4> rgba_output;

[[vk::push_constant]] ConstantBuffer<ColorConverterPushConstants> pc;

uint read_byte(uint byte_offset) {
    return (nv12_input[byte_offset >> 2u] >> (8u * (byte_offset & 3u))) & 0xFFu;
}

[numthreads(16, 16, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint2 pos = invocation.xy;
    if (pos.x >= pc.width || pos.y >= pc.height) return;

    // Y plane: stride may exceed width (V4L2 `bytesperline` padding).
    uint y_offset = pos.y * pc.plane0_stride_bytes + pos.x;
    float Y = float(read_byte(y_offset));

    // UV plane: interleaved (U, V) at half spatial resolution. Starts
    // at `plane1_offset_bytes` and uses `plane1_stride_bytes` row
    // stride — both come from V4L2, not derived from width.
    uint uv_offset = pc.plane1_offset_bytes
                   + (pos.y >> 1u) * pc.plane1_stride_bytes
                   + (pos.x & ~1u);
    float U = float(read_byte(uv_offset));
    float V = float(read_byte(uv_offset + 1u));

    rgba_output.Store(pos, float4(convert_color(float3(Y, U, V), pc), 1.0));
}
