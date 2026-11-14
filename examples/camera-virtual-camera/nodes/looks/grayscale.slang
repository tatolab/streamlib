// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

// Black and white: every pixel becomes its own brightness.

float4 effect(float4 source, int2 at) {
    // BT.709 luma, the weights the HD standard published for it.
    float luma = dot(source.rgb, float3(0.2126, 0.7152, 0.0722));
    return float4(float3(luma), source.a);
}
