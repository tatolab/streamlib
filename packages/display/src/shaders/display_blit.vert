// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#version 450

layout(location = 0) out vec2 outUV;

void main() {
    // Fullscreen triangle trick: 3 vertices covering the entire viewport.
    // No vertex buffer needed — UVs derived from gl_VertexIndex.
    //   vertex 0: pos(-1,-1), uv(0,0)  — bottom-left
    //   vertex 1: pos( 3,-1), uv(2,0)  — far right
    //   vertex 2: pos(-1, 3), uv(0,2)  — far top
    outUV = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    gl_Position = vec4(outUV * 2.0 - 1.0, 0.0, 1.0);
}
