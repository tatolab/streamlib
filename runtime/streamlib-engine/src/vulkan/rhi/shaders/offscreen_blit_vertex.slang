// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

struct OffscreenBlitVertexOutput {
    float4 position : SV_Position;
    [[vk::location(0)]] float2 outUV : TEXCOORD0;
};

OffscreenBlitVertexOutput main(uint vertex_index : SV_VulkanVertexID) {
    // Fullscreen triangle trick: 3 vertices covering the entire viewport.
    // No vertex buffer needed — UVs derived from the vertex index.
    //   vertex 0: pos(-1,-1), uv(0,0)  — bottom-left
    //   vertex 1: pos( 3,-1), uv(2,0)  — far right
    //   vertex 2: pos(-1, 3), uv(0,2)  — far top
    OffscreenBlitVertexOutput output;
    output.outUV = float2((vertex_index << 1u) & 2u, vertex_index & 2u);
    output.position = float4(output.outUV * 2.0 - 1.0, 0.0, 1.0);
    return output;
}
