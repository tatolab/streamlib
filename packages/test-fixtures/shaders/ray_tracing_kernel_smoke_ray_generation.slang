// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only
//
// Cdylib ray-tracing-kernel smoke test — ray-generation stage.
// Traces a single ray per pixel against the bound TLAS and writes
// the bary-coord hit color into the storage image. Push-constant
// variant gate proves the push-constant wiring works end-to-end.
// Smoke-only — pixel correctness is not asserted.

[[vk::binding(0, 0)]] RaytracingAccelerationStructure topLevelAS;
[[vk::binding(1, 0)]] [format("rgba8")] WTexture2D<float4> outputImage;

struct RayTracingKernelSmokePushConstants {
    uint variant;
};
[[vk::push_constant]] ConstantBuffer<RayTracingKernelSmokePushConstants> pc;

struct RayTracingKernelSmokePayload {
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

    RayTracingKernelSmokePayload payload;
    payload.hitColor = float3(0.0);
    TraceRay(topLevelAS, RAY_FLAG_FORCE_OPAQUE, 0xff, 0, 0, 0, ray, payload);

    // variant=0 → write hit color as-is; variant=1 → invert.
    float3 color = (pc.variant == 0u) ? payload.hitColor : (float3(1.0) - payload.hitColor);
    outputImage.Store(launch_id, float4(color, 1.0));
}
