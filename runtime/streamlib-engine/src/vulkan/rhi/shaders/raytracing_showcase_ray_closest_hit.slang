// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

struct RayTracingShowcasePayload {
    float3 payloadColor;
};

void main(inout RayTracingShowcasePayload payload, BuiltInTriangleIntersectionAttributes hit) {
    float2 attribs = hit.barycentrics;
    float3 bary = float3(1.0 - attribs.x - attribs.y, attribs.x, attribs.y);

    uint instanceId = InstanceID();
    float3 perInstance = float3(
        frac(float(instanceId) * 0.61803 + 0.10),
        frac(float(instanceId) * 0.31415 + 0.55),
        frac(float(instanceId) * 0.27182 + 0.30)
    );

    float3 lightDir = normalize(float3(0.6, 1.0, 0.4));
    float3 normal = normalize(cross(
        WorldRayDirection().zxy,
        WorldRayDirection()
    ));
    float ndotl = max(dot(normalize(normal), lightDir), 0.0);
    float ambient = 0.25;

    payload.payloadColor = perInstance * (ambient + (1.0 - ambient) * ndotl) * (0.6 + 0.4 * bary.x);
}
