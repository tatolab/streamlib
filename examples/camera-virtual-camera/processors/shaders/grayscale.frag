// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Black and white: every pixel becomes its own brightness.

#version 450

layout(location = 0) in vec2 screen_uv;
layout(location = 0) out vec4 painted_colour;

layout(set = 0, binding = 0) uniform sampler2D upstream_frame;

void main() {
    vec4 source = texture(upstream_frame, screen_uv);
    // BT.709 luma, the weights the HD standard published for it.
    float luma = dot(source.rgb, vec3(0.2126, 0.7152, 0.0722));
    painted_colour = vec4(vec3(luma), source.a);
}
