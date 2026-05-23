// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Minimal fragment shader: outputs constant color. No bindings, no
// push constants. Pairs with trivial_vert.vert for the
// `VulkanGraphicsKernel` β-shape Create+Clone+Drop round-trip.

#version 450

layout(location = 0) out vec4 outColor;

void main() {
    outColor = vec4(1.0, 0.5, 0.0, 1.0);
}
