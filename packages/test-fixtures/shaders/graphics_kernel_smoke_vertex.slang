// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only
//
// Cdylib graphics-kernel smoke test — vertex stage.
// Fabricates a single centered triangle from the vertex index (no vertex
// buffer required). Single fragment-only push-constant variant gate
// proves the push-constant wiring works end-to-end.

struct GraphicsKernelSmokePushConstants {
    uint variant;
};
[[vk::push_constant]] ConstantBuffer<GraphicsKernelSmokePushConstants> pc;

static const float2 TRIANGLE_POSITIONS[3] = {
    float2( 0.0, -0.5),
    float2(-0.5,  0.5),
    float2( 0.5,  0.5)
};

float4 main(uint vertex_index : SV_VulkanVertexID) : SV_Position {
    return float4(TRIANGLE_POSITIONS[vertex_index], 0.0, 1.0);
}
