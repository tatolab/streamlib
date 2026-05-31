// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Grayscale fragment shader. Samples the input texture (descriptor-set 0,
// binding 0) and writes the BT.601 luma of each texel to all three color
// channels, matching the macOS CoreVideo CPU path
// (gray = 0.299*R + 0.587*G + 0.114*B). The hardware sampler handles
// tiled-format access. Output goes to the bound color attachment.

#version 450

layout(location = 0) in vec2 inUV;
layout(location = 0) out vec4 outColor;

layout(set = 0, binding = 0) uniform sampler2D inputTex;

void main() {
    vec4 src = texture(inputTex, inUV);
    float luma = dot(src.rgb, vec3(0.299, 0.587, 0.114));
    outColor = vec4(luma, luma, luma, 1.0);
}
