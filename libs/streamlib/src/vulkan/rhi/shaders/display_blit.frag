// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#version 450

layout(location = 0) in vec2 inUV;
layout(location = 0) out vec4 outColor;

layout(set = 0, binding = 0) uniform sampler2D cameraTexture;

layout(push_constant) uniform PushConstants {
    vec2 scale;
    vec2 offset;
} pc;

void main() {
    // Aspect-ratio-aware sampling with letterbox/pillarbox black bars
    vec2 texCoord = (inUV - 0.5) / pc.scale + 0.5 + pc.offset;

    if (texCoord.x < 0.0 || texCoord.x > 1.0 || texCoord.y < 0.0 || texCoord.y > 1.0) {
        outColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }

    outColor = texture(cameraTexture, texCoord);
}
