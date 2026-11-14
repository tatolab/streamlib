// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only
//
// Cdylib ray-tracing-kernel smoke test — miss stage.

struct RayTracingKernelSmokePayload {
    float3 hitColor;
};

void main(inout RayTracingKernelSmokePayload payload) {
    payload.hitColor = float3(0.05, 0.05, 0.20);
}
