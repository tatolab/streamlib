// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Cdylib ray-tracing-kernel smoke test — closest-hit stage.
// Writes the bary-coord weights as the hit color.

struct RayTracingKernelSmokePayload {
    float3 hitColor;
};

void main(inout RayTracingKernelSmokePayload payload, BuiltInTriangleIntersectionAttributes hit) {
    float2 attribs = hit.barycentrics;
    payload.hitColor = float3(1.0 - attribs.x - attribs.y, attribs.x, attribs.y);
}
