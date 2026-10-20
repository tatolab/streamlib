# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The one lens model the fisheye warp and its inverse are both built from.

`RADIAL_DISTORTION_MODEL_GLSL` is the prelude both shaders start with, so the
lens and the rectifier compute the same radius from the same pixel and scale it
by the same polynomial. Not a node: the two node modules import it.
"""

from __future__ import annotations

__all__ = [
    "DEFAULT_RADIAL_DISTORTION_K1",
    "DEFAULT_RADIAL_DISTORTION_K2",
    "FRAME_TEXTURE_FORMAT",
    "RADIAL_DISTORTION_MODEL_GLSL",
    "STORAGE_AND_SAMPLED_TEXTURE_USAGE",
    "WORKGROUP_TILE_SIZE",
    "largest_recoverable_normalised_radius",
    "workgroups_covering",
]

# The radial-distortion coefficients the lens applies and the rectifier
# inverts. Negative `k1` is the barrel direction.
DEFAULT_RADIAL_DISTORTION_K1 = -0.25
DEFAULT_RADIAL_DISTORTION_K2 = 0.0

# Every frame both kernels land, sample and write is RGBA8.
FRAME_TEXTURE_FORMAT = "rgba8_unorm"

# A kernel's output: written as a storage image, then sampled downstream.
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

# The dispatch's tile and the shader's `local_size`, one number for both: a
# shader tile larger than the dispatch's leaves the right and bottom of every
# frame unwritten, and nothing refuses it.
WORKGROUP_TILE_SIZE = 8

RADIAL_DISTORTION_MODEL_GLSL = (
    f"#version 450\n#define WORKGROUP_TILE_SIZE {WORKGROUP_TILE_SIZE}\n"
    """
layout(local_size_x = WORKGROUP_TILE_SIZE, local_size_y = WORKGROUP_TILE_SIZE) in;

vec2 frame_centre(ivec2 extent) {
    return (vec2(extent) - 1.0) * 0.5;
}

// Normalised against the half-diagonal, so the corners of any aspect ratio sit
// at r = 1, inside the range the coefficients describe.
float normalised_radius(ivec2 at, ivec2 extent) {
    vec2 centre = frame_centre(extent);
    return length(vec2(at) - centre) / length(centre);
}

// The factor a point's distance from the centre is multiplied by. Negative k1
// is the barrel direction.
float radial_scale(float radius, float k1, float k2) {
    float radius_squared = radius * radius;
    return 1.0 + k1 * radius_squared + k2 * radius_squared * radius_squared;
}

// Texel `i`'s centre is at (i + 0.5) / extent in sampler coordinates; the
// engine's sampler filters linearly between texels.
vec2 texel_to_sampler_coordinates(vec2 texel, ivec2 extent) {
    return (texel + 0.5) / vec2(extent);
}
"""
)


def workgroups_covering(pixels: int) -> int:
    """How many tiles it takes to cover `pixels`, the last one hanging over."""
    return (pixels + WORKGROUP_TILE_SIZE - 1) // WORKGROUP_TILE_SIZE


# The swept function is a cubic in r² with one interior maximum; this many
# samples place it to a fraction of a pixel at any capture size.
RECOVERABLE_RADIUS_SWEEP_STEPS = 1024


def largest_recoverable_normalised_radius(k1: float, k2: float) -> float:
    """The normalised radius past which the warp carried nothing across, so no inverse recovers it.

    The warp writes radius `r` from radius `r * radial_scale(r)`; under a
    barrel the whole frame is sampled from the disc this returns.
    """
    largest = 0.0
    for step in range(RECOVERABLE_RADIUS_SWEEP_STEPS + 1):
        radius = step / RECOVERABLE_RADIUS_SWEEP_STEPS
        radius_squared = radius * radius
        sampled_from = radius * (
            1.0 + k1 * radius_squared + k2 * radius_squared * radius_squared
        )
        largest = max(largest, sampled_from)
    return largest
