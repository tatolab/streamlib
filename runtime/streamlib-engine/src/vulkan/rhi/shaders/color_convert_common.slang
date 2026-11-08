// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Shared math for color conversion compute shaders. Mirrored CPU-side
// in `runtime/streamlib-engine/src/core/color/transfer.rs` — the round-
// trip and "known points" unit tests on the CPU side are what guard
// drift between this file and the Rust reference.

// Keep in sync with `core::color::transfer::TransferId`.
static const uint TRANSFER_LINEAR = 0u;
static const uint TRANSFER_SRGB = 1u;
static const uint TRANSFER_BT709 = 2u;
static const uint TRANSFER_PQ = 3u;
static const uint TRANSFER_HLG = 4u;

// Keep in sync with `core::rhi::color_converter::ColorConverterPushConstants::FLAG_APPLY_TRANSFER`.
static const uint FLAG_APPLY_TRANSFER = 1u;

// The push constants every color converter takes — see
// `core::rhi::color_converter::ColorConverterPushConstants`.
struct ColorConverterPushConstants {
    float4 matrix_row0;  // .xyz = row of the conversion matrix, .w = pad
    float4 matrix_row1;
    float4 matrix_row2;
    float4 range_offset; // .xyz = byte-domain (y, cb, cr) offset, .w = pad
    uint width;
    uint height;
    uint transfer_in;
    uint transfer_out;
    uint flags;
    uint plane0_stride_bytes;
    uint plane1_stride_bytes;
    uint plane1_offset_bytes;
};

float transfer_to_linear(uint id, float x) {
    if (id == TRANSFER_SRGB) {
        return x <= 0.04045 ? x / 12.92 : pow((x + 0.055) / 1.055, 2.4);
    }
    if (id == TRANSFER_BT709) {
        return x < 0.081 ? x / 4.5 : pow((x + 0.099) / 1.099, 1.0 / 0.45);
    }
    if (id == TRANSFER_PQ) {
        const float m1 = 2610.0 / 16384.0;
        const float m2 = (2523.0 / 4096.0) * 128.0;
        const float c1 = 3424.0 / 4096.0;
        const float c2 = (2413.0 / 4096.0) * 32.0;
        const float c3 = (2392.0 / 4096.0) * 32.0;
        float xp = pow(max(x, 0.0), 1.0 / m2);
        float num = max(xp - c1, 0.0);
        float den = c2 - c3 * xp;
        return pow(num / den, 1.0 / m1);
    }
    if (id == TRANSFER_HLG) {
        const float a = 0.17883277;
        const float b = 0.28466892;
        const float c = 0.55991073;
        return x <= 0.5 ? (x * x) / 3.0 : (exp((x - c) / a) + b) / 12.0;
    }
    // TRANSFER_LINEAR (and any unrecognized id) → identity.
    return x;
}

float transfer_from_linear(uint id, float x) {
    if (id == TRANSFER_SRGB) {
        return x <= 0.0031308 ? 12.92 * x : 1.055 * pow(x, 1.0 / 2.4) - 0.055;
    }
    if (id == TRANSFER_BT709) {
        return x < 0.018 ? 4.5 * x : 1.099 * pow(x, 0.45) - 0.099;
    }
    if (id == TRANSFER_PQ) {
        const float m1 = 2610.0 / 16384.0;
        const float m2 = (2523.0 / 4096.0) * 128.0;
        const float c1 = 3424.0 / 4096.0;
        const float c2 = (2413.0 / 4096.0) * 32.0;
        const float c3 = (2392.0 / 4096.0) * 32.0;
        float xp = pow(max(x, 0.0), m1);
        float num = c1 + c2 * xp;
        float den = 1.0 + c3 * xp;
        return pow(num / den, m2);
    }
    if (id == TRANSFER_HLG) {
        const float a = 0.17883277;
        const float b = 0.28466892;
        const float c = 0.55991073;
        return x <= 1.0 / 12.0 ? sqrt(3.0 * max(x, 0.0))
                               : a * log(12.0 * x - b) + c;
    }
    return x;
}

// Each channel's transfer-in EOTF, then its transfer-out OETF.
float3 retransferred(float3 rgb, uint transfer_in, uint transfer_out) {
    return float3(
        transfer_from_linear(transfer_out, transfer_to_linear(transfer_in, rgb.r)),
        transfer_from_linear(transfer_out, transfer_to_linear(transfer_in, rgb.g)),
        transfer_from_linear(transfer_out, transfer_to_linear(transfer_in, rgb.b)));
}

// Closed-form `ycbcr_byte → rgba_normalized` pipeline:
//   1. Subtract `range_offset` from raw byte-domain YCbCr.
//   2. Multiply by the (row-major) 3×3 matrix; the matrix bakes in
//      range expansion (Y scale = 255/219 for limited-range, 1 for
//      full-range; chroma scale = 255/224 for limited).
//   3. Divide by 255 + clamp to `[0, 1]`.
//   4. If `FLAG_APPLY_TRANSFER` is set, run the transfer-in EOTF then
//      transfer-out OETF closed-form on each channel.
float3 convert_color(float3 ycbcr_byte, ColorConverterPushConstants conversion) {
    float3 c = ycbcr_byte - conversion.range_offset.xyz;
    float3 rgb_byte = float3(
        dot(conversion.matrix_row0.xyz, c),
        dot(conversion.matrix_row1.xyz, c),
        dot(conversion.matrix_row2.xyz, c));
    float3 rgb = clamp(rgb_byte / 255.0, 0.0, 1.0);
    if ((conversion.flags & FLAG_APPLY_TRANSFER) != 0u) {
        rgb = retransferred(rgb, conversion.transfer_in, conversion.transfer_out);
    }
    return rgb;
}

// Closed-form `rgb_normalized → ycbcr_byte` pipeline, the inverse of
// `convert_color`:
//   1. If `FLAG_APPLY_TRANSFER` is set, run the transfer-in EOTF then
//      transfer-out OETF closed-form on each channel.
//   2. Clamp to `[0, 1]` and scale to the byte domain.
//   3. Multiply by the (row-major) RGB→YCbCr 3×3 matrix, add
//      `range_offset`, and clamp to `[0, 255]`.
float3 convert_color_to_ycbcr_bytes(float3 rgb, ColorConverterPushConstants conversion) {
    if ((conversion.flags & FLAG_APPLY_TRANSFER) != 0u) {
        rgb = retransferred(rgb, conversion.transfer_in, conversion.transfer_out);
    }
    float3 rgb_byte = clamp(rgb, 0.0, 1.0) * 255.0;
    float3 ycbcr = float3(
        dot(conversion.matrix_row0.xyz, rgb_byte),
        dot(conversion.matrix_row1.xyz, rgb_byte),
        dot(conversion.matrix_row2.xyz, rgb_byte))
        + conversion.range_offset.xyz;
    return clamp(ycbcr, 0.0, 255.0);
}
