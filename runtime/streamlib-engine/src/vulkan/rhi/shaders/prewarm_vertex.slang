// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only
//
// Trivial vertex stage for the graphics-pipeline compiler pre-warm
// (`HostVulkanDevice::prewarm_graphics_pipeline`). No vertex input — the
// fullscreen-triangle positions come from the vertex index. Paired with
// prewarm_fragment.slang purely to force the first vkCreateGraphicsPipelines so the
// driver's shader-compiler init runs single-threaded at device construction.

float4 main(uint vertex_index : SV_VulkanVertexID) : SV_Position {
    float2 p = float2((vertex_index << 1u) & 2u, vertex_index & 2u);
    return float4(p * 2.0 - 1.0, 0.0, 1.0);
}
