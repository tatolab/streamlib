// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

struct RayTracingShowcasePayload {
    float3 payloadColor;
};

void main(inout RayTracingShowcasePayload payload) {
    float t = clamp(WorldRayDirection().y * 0.5 + 0.5, 0.0, 1.0);
    float3 zenith = float3(0.05, 0.10, 0.40);
    float3 horizon = float3(0.85, 0.55, 0.35);
    payload.payloadColor = lerp(horizon, zenith, t);
}
