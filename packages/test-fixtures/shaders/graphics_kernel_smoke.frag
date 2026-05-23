// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Cdylib graphics-kernel smoke test — fragment stage.
// Single Rgba8Unorm color attachment. The push-constant variant gates
// the output color (variant=0 → red; variant=1 → cyan). The smoke
// test only round-trips kernel construction + setter dispatch + a
// single offscreen render — visual correctness is not asserted, so
// the colors are arbitrary but distinct.

#version 450

layout(push_constant) uniform Pc {
    uint variant;
} pc;

layout(location = 0) out vec4 out_color;

void main() {
    out_color = (pc.variant == 0u)
        ? vec4(1.0, 0.0, 0.0, 1.0)
        : vec4(0.0, 1.0, 1.0, 1.0);
}
