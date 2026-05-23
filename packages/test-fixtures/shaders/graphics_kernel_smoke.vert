// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Cdylib graphics-kernel smoke test — vertex stage.
// Fabricates a single centered triangle from gl_VertexIndex (no vertex
// buffer required). Single fragment-only push-constant variant gate
// proves the push-constant slot vtable wiring works end-to-end.

#version 450

layout(push_constant) uniform Pc {
    uint variant;
} pc;

void main() {
    vec2 positions[3] = vec2[3](
        vec2( 0.0, -0.5),
        vec2(-0.5,  0.5),
        vec2( 0.5,  0.5)
    );
    gl_Position = vec4(positions[gl_VertexIndex], 0.0, 1.0);
}
