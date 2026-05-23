// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Cdylib ray-tracing-kernel smoke test — closest-hit stage.
// Writes the bary-coord weights as the hit color.

#version 460
#extension GL_EXT_ray_tracing : require

layout(location = 0) rayPayloadInEXT vec3 hitColor;
hitAttributeEXT vec2 attribs;

void main() {
    vec3 bary = vec3(1.0 - attribs.x - attribs.y, attribs.x, attribs.y);
    hitColor = bary;
}
