// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

struct RayTracingTestPayload {
    float3 hitColor;
};

void main(inout RayTracingTestPayload payload) {
    payload.hitColor = float3(0.05, 0.05, 0.20);
}
