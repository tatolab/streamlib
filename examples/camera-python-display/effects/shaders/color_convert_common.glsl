// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Shared math for color conversion compute shaders. Example-local copy of the
// engine's `runtime/streamlib-engine/src/vulkan/rhi/shaders/color_convert_common.glsl`
// — the sandboxed tone-mapper (`tone_curve.comp`) rides the engine-free plugin
// SDK, so its shader math lives in the example rather than reaching engine
// internals. The 32-byte push-constant layout lock in `tone_mapper.rs` guards
// drift on the Rust side; the `TransferId` enum values are shared with
// `streamlib_plugin_sdk::sdk::color::TransferId`.

// Keep in sync with `streamlib_plugin_sdk::sdk::color::TransferId`.
const uint TRANSFER_LINEAR = 0u;
const uint TRANSFER_SRGB = 1u;
const uint TRANSFER_BT709 = 2u;
const uint TRANSFER_PQ = 3u;
const uint TRANSFER_HLG = 4u;

// Keep in sync with `ColorConverterPushConstants::FLAG_APPLY_TRANSFER`.
const uint FLAG_APPLY_TRANSFER = 1u;

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

// Closed-form `ycbcr_byte → rgba_normalized` pipeline:
//   1. Subtract `range_offset` from raw byte-domain YCbCr.
//   2. Multiply by the (row-major) 3×3 matrix; the matrix bakes in
//      range expansion (Y scale = 255/219 for limited-range, 1 for
//      full-range; chroma scale = 255/224 for limited).
//   3. Divide by 255 + clamp to `[0, 1]`.
//   4. If `FLAG_APPLY_TRANSFER` is set, run the transfer-in EOTF then
//      transfer-out OETF closed-form on each channel.
vec3 convert_color(
    vec3 ycbcr_byte,
    vec3 row0,
    vec3 row1,
    vec3 row2,
    vec3 range_offset,
    uint transfer_in,
    uint transfer_out,
    uint flags
) {
    vec3 c = ycbcr_byte - range_offset;
    vec3 rgb_byte = vec3(dot(row0, c), dot(row1, c), dot(row2, c));
    vec3 rgb = clamp(rgb_byte / 255.0, 0.0, 1.0);
    if ((flags & FLAG_APPLY_TRANSFER) != 0u) {
        rgb.r = transfer_from_linear(transfer_out, transfer_to_linear(transfer_in, rgb.r));
        rgb.g = transfer_from_linear(transfer_out, transfer_to_linear(transfer_in, rgb.g));
        rgb.b = transfer_from_linear(transfer_out, transfer_to_linear(transfer_in, rgb.b));
    }
    return rgb;
}
