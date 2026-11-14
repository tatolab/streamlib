// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

[[vk::binding(0, 0)]] RaytracingAccelerationStructure topLevelAS;
[[vk::binding(1, 0)]] [format("rgba8")] WTexture2D<float4> outputImage;

struct RayTracingTestPayload {
    float3 hitColor;
};

void main() {
    uint2 launch_id = DispatchRaysIndex().xy;
    float2 pixelCenter = float2(launch_id) + float2(0.5);
    float2 uv = pixelCenter / float2(DispatchRaysDimensions().xy);
    float2 ndc = uv * 2.0 - 1.0;

    RayDesc ray;
    ray.Origin = float3(ndc.x, -ndc.y, 1.0);
    ray.TMin = 0.001;
    ray.Direction = float3(0.0, 0.0, -1.0);
    ray.TMax = 100.0;

    RayTracingTestPayload payload;
    payload.hitColor = float3(0.0);
    TraceRay(topLevelAS, RAY_FLAG_FORCE_OPAQUE, 0xff, 0, 0, 0, ray, payload);

    outputImage.Store(launch_id, float4(payload.hitColor, 1.0));
}
