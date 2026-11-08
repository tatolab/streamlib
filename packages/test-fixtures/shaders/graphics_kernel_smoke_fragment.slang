// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Cdylib graphics-kernel smoke test — fragment stage.
// Single Rgba8Unorm color attachment. The push-constant variant gates
// the output color (variant=0 → red; variant=1 → cyan). The smoke
// test only round-trips kernel construction + setter dispatch + a
// single offscreen render — visual correctness is not asserted, so
// the colors are arbitrary but distinct.

struct GraphicsKernelSmokePushConstants {
    uint variant;
};
[[vk::push_constant]] ConstantBuffer<GraphicsKernelSmokePushConstants> pc;

float4 main() : SV_Target0 {
    return (pc.variant == 0u)
        ? float4(1.0, 0.0, 0.0, 1.0)
        : float4(0.0, 1.0, 1.0, 1.0);
}
