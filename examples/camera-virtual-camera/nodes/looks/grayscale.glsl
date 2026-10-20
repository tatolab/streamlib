// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Black and white: every pixel becomes its own brightness.

vec4 effect(vec4 source, ivec2 at) {
    // BT.709 luma, the weights the HD standard published for it.
    float luma = dot(source.rgb, vec3(0.2126, 0.7152, 0.0722));
    return vec4(vec3(luma), source.a);
}
