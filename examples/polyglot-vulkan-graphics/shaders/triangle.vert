// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Polyglot Vulkan adapter graphics scenario (#656) — vertex stage.
// Fabricates a centered triangle from gl_VertexIndex (no vertex buffer
// required). Pushes a per-vertex color the fragment stage interpolates.

#version 450

layout(push_constant) uniform Pc {
    uint variant;
} pc;

layout(location = 0) out vec3 v_color;

void main() {
    // Centered triangle, ~60% of viewport.
    vec2 positions[3] = vec2[3](
        vec2( 0.0, -0.6),
        vec2(-0.6,  0.6),
        vec2( 0.6,  0.6)
    );

    // Variant 0: Python palette (R / G / B). Variant 1: Deno palette
    // (cyan / magenta / yellow). Different palettes give the host's PNG
    // readback a visually distinct gate per runtime so the Read-tool
    // check confirms which subprocess actually ran.
    vec3 colors;
    if (pc.variant == 0u) {
        vec3 py[3] = vec3[3](
            vec3(1.0, 0.0, 0.0),
            vec3(0.0, 1.0, 0.0),
            vec3(0.0, 0.0, 1.0)
        );
        colors = py[gl_VertexIndex];
    } else {
        vec3 dn[3] = vec3[3](
            vec3(0.0, 1.0, 1.0),
            vec3(1.0, 0.0, 1.0),
            vec3(1.0, 1.0, 0.0)
        );
        colors = dn[gl_VertexIndex];
    }

    gl_Position = vec4(positions[gl_VertexIndex], 0.0, 1.0);
    v_color = colors;
}
