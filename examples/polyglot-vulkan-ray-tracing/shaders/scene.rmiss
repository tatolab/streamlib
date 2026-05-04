// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Polyglot Vulkan ray-tracing scenario (#667) — miss stage. Picks the
// gradient palette from the `variant` push-constant so the PNGs the
// host writes are visually distinct between Python (variant 0) and
// Deno (variant 1) runs.

#version 460
#extension GL_EXT_ray_tracing : require

layout(push_constant) uniform Pc {
    uint variant;
} pc;

layout(location = 0) rayPayloadInEXT vec3 payloadColor;

void main() {
    float t = clamp(gl_WorldRayDirectionEXT.y * 0.5 + 0.5, 0.0, 1.0);
    if (pc.variant == 0u) {
        // Python palette: teal zenith → magenta horizon.
        vec3 zenith = vec3(0.10, 0.55, 0.55);
        vec3 horizon = vec3(0.85, 0.30, 0.55);
        payloadColor = mix(horizon, zenith, t);
    } else {
        // Deno palette: violet zenith → orange horizon.
        vec3 zenith = vec3(0.20, 0.10, 0.55);
        vec3 horizon = vec3(0.95, 0.55, 0.30);
        payloadColor = mix(horizon, zenith, t);
    }
}
