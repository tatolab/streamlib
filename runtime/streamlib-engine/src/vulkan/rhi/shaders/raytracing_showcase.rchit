// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#version 460
#extension GL_EXT_ray_tracing : require

layout(location = 0) rayPayloadInEXT vec3 payloadColor;
hitAttributeEXT vec2 attribs;

void main() {
    vec3 bary = vec3(1.0 - attribs.x - attribs.y, attribs.x, attribs.y);

    uint instanceId = gl_InstanceCustomIndexEXT;
    vec3 perInstance = vec3(
        fract(float(instanceId) * 0.61803 + 0.10),
        fract(float(instanceId) * 0.31415 + 0.55),
        fract(float(instanceId) * 0.27182 + 0.30)
    );

    vec3 lightDir = normalize(vec3(0.6, 1.0, 0.4));
    vec3 normal = normalize(cross(
        gl_WorldRayDirectionEXT.zxy,
        gl_WorldRayDirectionEXT
    ));
    float ndotl = max(dot(normalize(normal), lightDir), 0.0);
    float ambient = 0.25;

    payloadColor = perInstance * (ambient + (1.0 - ambient) * ndotl) * (0.6 + 0.4 * bary.x);
}
