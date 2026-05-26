// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Polyglot Vulkan adapter graphics scenario (#656) — fragment stage.
// Pass-through of the interpolated per-vertex color from the vertex
// stage. Single Rgba8Unorm color attachment.

#version 450

layout(location = 0) in vec3 v_color;
layout(location = 0) out vec4 out_color;

void main() {
    out_color = vec4(v_color, 1.0);
}
