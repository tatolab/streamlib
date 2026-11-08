// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

struct RayTracingTestPayload {
    float3 hitColor;
};

void main(inout RayTracingTestPayload payload) {
    payload.hitColor = float3(0.05, 0.05, 0.20);
}
