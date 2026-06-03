// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Trivial vertex stage for the graphics-pipeline compiler pre-warm
// (`HostVulkanDevice::prewarm_graphics_pipeline`). No vertex input — the
// fullscreen-triangle positions come from gl_VertexIndex. Paired with
// prewarm.frag purely to force the first vkCreateGraphicsPipelines so the
// driver's shader-compiler init runs single-threaded at device construction.
#version 450

void main() {
    vec2 p = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
