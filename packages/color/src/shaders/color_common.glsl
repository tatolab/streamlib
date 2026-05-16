// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Shared math for color compute shaders in @tatolab/color.
//
// Mirror of the subset of `libs/streamlib-engine/src/vulkan/rhi/shaders/
// color_convert_common.glsl` that this package's shaders need
// (TransferId constants + closed-form EOTF/OETF). When engine color
// math fully migrates here, the two files consolidate.

// Keep in sync with `crate::transfer::TransferId`.
const uint TRANSFER_LINEAR = 0u;
const uint TRANSFER_SRGB = 1u;
const uint TRANSFER_BT709 = 2u;
const uint TRANSFER_PQ = 3u;
const uint TRANSFER_HLG = 4u;

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
