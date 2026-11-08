// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Cdylib ray-tracing-kernel smoke test — miss stage.

struct RayTracingKernelSmokePayload {
    float3 hitColor;
};

void main(inout RayTracingKernelSmokePayload payload) {
    payload.hitColor = float3(0.05, 0.05, 0.20);
}
