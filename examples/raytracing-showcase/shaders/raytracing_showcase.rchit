// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#version 460
#extension GL_EXT_ray_tracing : require

layout(location = 0) rayPayloadInEXT vec3 payloadColor;
hitAttributeEXT vec2 attribs;

void main() {
    // Hit position in world space.
    vec3 hitPos = gl_WorldRayOriginEXT + gl_HitTEXT * gl_WorldRayDirectionEXT;

    // The unit cube spans [-0.5, 0.5]^3. Whichever axis is closest to
    // ±0.5 is the face we hit; derive the face normal from that.
    vec3 absPos = abs(hitPos);
    vec3 normal;
    vec3 faceColor;
    if (absPos.x > absPos.y && absPos.x > absPos.z) {
        normal = vec3(sign(hitPos.x), 0.0, 0.0);
        faceColor = hitPos.x > 0.0
            ? vec3(0.95, 0.30, 0.30)   // +X red
            : vec3(0.40, 0.80, 0.95);  // -X cyan
    } else if (absPos.y > absPos.z) {
        normal = vec3(0.0, sign(hitPos.y), 0.0);
        faceColor = hitPos.y > 0.0
            ? vec3(0.30, 0.95, 0.30)   // +Y green
            : vec3(0.95, 0.85, 0.40);  // -Y yellow
    } else {
        normal = vec3(0.0, 0.0, sign(hitPos.z));
        faceColor = hitPos.z > 0.0
            ? vec3(0.55, 0.40, 0.95)   // +Z purple
            : vec3(0.95, 0.55, 0.40);  // -Z orange
    }

    // Single directional light + small ambient.
    vec3 lightDir = normalize(vec3(0.5, 0.9, 0.4));
    float ambient = 0.25;
    float ndotl = max(dot(normal, lightDir), 0.0);
    float lighting = ambient + (1.0 - ambient) * ndotl;

    // Subtle barycentric edge tint to give the cube some texture.
    vec3 bary = vec3(1.0 - attribs.x - attribs.y, attribs.x, attribs.y);
    float edge = smoothstep(0.0, 0.04, min(min(bary.x, bary.y), bary.z));
    vec3 edgeShade = mix(vec3(0.0), vec3(1.0), edge);

    payloadColor = faceColor * lighting * edgeShade;
}
