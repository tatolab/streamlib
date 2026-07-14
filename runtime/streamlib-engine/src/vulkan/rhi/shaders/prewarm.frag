// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Trivial fragment stage for the graphics-pipeline compiler pre-warm. Reads
// the push constant so the stage isn't optimized away and the driver can't
// fast-path the compile. See prewarm.vert / prewarm_graphics_pipeline.
#version 450

layout(location = 0) out vec4 color;
layout(push_constant) uniform PushConstants { uint v; } pc;

void main() {
    color = vec4(float(pc.v & 0xFFu) / 255.0);
}
