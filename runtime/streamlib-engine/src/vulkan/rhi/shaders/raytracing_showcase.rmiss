// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#version 460
#extension GL_EXT_ray_tracing : require

layout(location = 0) rayPayloadInEXT vec3 payloadColor;

void main() {
    float t = clamp(gl_WorldRayDirectionEXT.y * 0.5 + 0.5, 0.0, 1.0);
    vec3 zenith = vec3(0.05, 0.10, 0.40);
    vec3 horizon = vec3(0.85, 0.55, 0.35);
    payloadColor = mix(horizon, zenith, t);
}
