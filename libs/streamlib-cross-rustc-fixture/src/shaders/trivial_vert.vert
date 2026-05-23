// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Minimal vertex shader: fullscreen triangle from gl_VertexIndex, no
// vertex inputs, no descriptor bindings. Paired with trivial_frag.frag
// to construct a `VulkanGraphicsKernel` for the cross-rustc β-shape
// Create+Clone+Drop round-trip — kernel construction is what we
// exercise; the pipeline is never actually drawn from.

#version 450

void main() {
    vec2 verts[3] = vec2[](
        vec2(-1.0, -1.0),
        vec2(3.0, -1.0),
        vec2(-1.0, 3.0)
    );
    gl_Position = vec4(verts[gl_VertexIndex], 0.0, 1.0);
}
