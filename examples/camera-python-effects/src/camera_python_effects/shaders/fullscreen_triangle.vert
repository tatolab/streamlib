// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// The vertex stage every pass in this app shares. No vertex buffer is
// reachable from a Python processor, so three vertices are fabricated from
// `gl_VertexIndex` and cover the whole viewport:
//   vertex 0: pos(-1,-1), uv(0,0)   vertex 1: pos(3,-1), uv(2,0)
//   vertex 2: pos(-1,3),  uv(0,2)

#version 450

layout(location = 0) out vec2 screen_uv;

void main() {
    screen_uv = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    gl_Position = vec4(screen_uv * 2.0 - 1.0, 0.0, 1.0);
}
