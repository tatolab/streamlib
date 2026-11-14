// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

struct RayTracingTestPayload {
    float3 hitColor;
};

void main(inout RayTracingTestPayload payload, BuiltInTriangleIntersectionAttributes hit) {
    float2 attribs = hit.barycentrics;
    payload.hitColor = float3(1.0 - attribs.x - attribs.y, attribs.x, attribs.y);
}
