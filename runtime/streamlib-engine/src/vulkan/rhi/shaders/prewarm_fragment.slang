// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only
//
// Trivial fragment stage for the graphics-pipeline compiler pre-warm. Reads
// the push constant so the stage isn't optimized away and the driver can't
// fast-path the compile. See prewarm_vertex.slang / prewarm_graphics_pipeline.

struct PrewarmFragmentPushConstants {
    uint v;
};
[[vk::push_constant]] ConstantBuffer<PrewarmFragmentPushConstants> pc;

float4 main() : SV_Target0 {
    return float4(float(pc.v & 0xFFu) / 255.0);
}
