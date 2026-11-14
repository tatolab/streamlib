// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

// The vertex stage every pass in this stream shares. No vertex buffer is
// reachable from a Python node, so three vertices are fabricated from
// `SV_VulkanVertexID` and cover the whole viewport:
//   vertex 0: pos(-1,-1), uv(0,0)   vertex 1: pos(3,-1), uv(2,0)
//   vertex 2: pos(-1,3),  uv(0,2)

struct FullscreenTriangleVertexToFragment {
    float4 clip_position : SV_Position;
    float2 screen_uv : SCREEN_UV;
};

FullscreenTriangleVertexToFragment main(uint vertex_index : SV_VulkanVertexID) {
    FullscreenTriangleVertexToFragment output;
    output.screen_uv = float2(float((vertex_index << 1) & 2u), float(vertex_index & 2u));
    output.clip_position = float4(output.screen_uv * 2.0 - 1.0, 0.0, 1.0);
    return output;
}
