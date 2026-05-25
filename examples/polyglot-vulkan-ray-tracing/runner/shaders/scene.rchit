// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Polyglot Vulkan ray-tracing scenario (#667) — closest-hit stage. The
// scene's single triangle gets a barycentric-weighted RGB palette so
// the visual gate is "the bright triangle in the middle has the three
// expected colors at its corners." The miss-shader gradient (variant-
// dependent) frames it.

#version 460
#extension GL_EXT_ray_tracing : require

layout(location = 0) rayPayloadInEXT vec3 payloadColor;
hitAttributeEXT vec2 attribs;

void main() {
    // Barycentric coords inside the hit triangle: (1-u-v, u, v) where
    // attribs = (u, v) from gl_HitAttributeEXT.
    vec3 bary = vec3(1.0 - attribs.x - attribs.y, attribs.x, attribs.y);
    // Color the corners red / green / blue, interpolated linearly across
    // the face — matches the vertex order in the BLAS and gives a
    // recognizable signal in the PNG.
    payloadColor = bary.x * vec3(1.00, 0.20, 0.20)
                 + bary.y * vec3(0.20, 1.00, 0.30)
                 + bary.z * vec3(0.30, 0.45, 1.00);
}
