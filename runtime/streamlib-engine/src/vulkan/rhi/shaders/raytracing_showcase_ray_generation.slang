// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

[[vk::binding(0, 0)]] RaytracingAccelerationStructure topLevelAS;
[[vk::binding(1, 0)]] [format("rgba8")] WTexture2D<float4> outputImage;

struct RayTracingShowcasePushConstants {
    float time;
    float aspect;
    int   width;
    int   height;
};
[[vk::push_constant]] ConstantBuffer<RayTracingShowcasePushConstants> pc;

struct RayTracingShowcasePayload {
    float3 payloadColor;
};

void main() {
    uint2 launch_id = DispatchRaysIndex().xy;
    float2 pixelCenter = float2(launch_id) + float2(0.5);
    float2 uv = pixelCenter / float2(DispatchRaysDimensions().xy);
    float2 ndc = uv * 2.0 - 1.0;
    ndc.x *= pc.aspect;
    ndc.y = -ndc.y;

    float r = 5.0;
    float3 cameraPos = float3(r * sin(pc.time), 1.5, r * cos(pc.time));
    float3 cameraTarget = float3(0.0, 0.0, 0.0);
    float3 worldUp = float3(0.0, 1.0, 0.0);

    float3 forward = normalize(cameraTarget - cameraPos);
    float3 right = normalize(cross(forward, worldUp));
    float3 up = cross(right, forward);

    float fovY = 0.9;
    float halfHeight = tan(fovY * 0.5);

    RayDesc ray;
    ray.Origin = cameraPos;
    ray.TMin = 0.001;
    ray.Direction = normalize(forward + right * (ndc.x * halfHeight) + up * (ndc.y * halfHeight));
    ray.TMax = 200.0;

    RayTracingShowcasePayload payload;
    payload.payloadColor = float3(0.0);
    TraceRay(topLevelAS, RAY_FLAG_FORCE_OPAQUE, 0xff, 0, 0, 0, ray, payload);

    outputImage.Store(launch_id, float4(payload.payloadColor, 1.0));
}
