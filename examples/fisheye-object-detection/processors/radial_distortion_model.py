# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""The lens model the warp and its inverse must agree on, in one place.

Two shaders in this app describe the same lens from opposite ends — one
applies the distortion, the other undoes it — and they are only each other's
inverse for as long as they compute the same radius from the same pixel and
scale it by the same polynomial. Kept apart, the pair drifts silently: a
rectifier a hair off its lens still produces a plausible-looking picture, and
the only symptom is a detector that quietly does worse near the edges. So the
model is one GLSL prelude both shaders are built from, and one Python function
that answers what the polynomial implies.
"""

from __future__ import annotations

__all__ = [
    "RADIAL_DISTORTION_MODEL_GLSL",
    "WORKGROUP_TILE_SIZE",
    "largest_recoverable_normalised_radius",
    "workgroups_covering",
]

# The dispatch asks for one workgroup per tile of this size, and the prelude
# declares the same size as its `local_size`. The two reach the shader as one
# number — see the `#define` below — because raising the Python without the
# GLSL leaves the right and bottom of every frame untouched and nothing
# anywhere refuses.
WORKGROUP_TILE_SIZE = 8

# The `#define` is the only interpolated line: the body stays a plain string,
# so the shader's own braces need no doubling and it reads as GLSL.
RADIAL_DISTORTION_MODEL_GLSL = (
    f"#version 450\n#define WORKGROUP_TILE_SIZE {WORKGROUP_TILE_SIZE}\n"
    """
layout(local_size_x = WORKGROUP_TILE_SIZE, local_size_y = WORKGROUP_TILE_SIZE) in;

vec2 frame_centre(ivec2 extent) {
    return (vec2(extent) - 1.0) * 0.5;
}

// Radius normalised against the half-diagonal, so the frame's corners sit at
// r = 1 whatever its aspect ratio. Normalising against the shorter half-axis
// instead — the obvious choice on a square image — puts the corners of a 16:9
// frame past r = 2, where the polynomial is far outside the range its
// coefficients were fitted over.
float normalised_radius(ivec2 at, ivec2 extent) {
    vec2 centre = frame_centre(extent);
    return length(vec2(at) - centre) / length(centre);
}

// The polynomial itself: the factor a point's distance from the centre is
// multiplied by. Negative k1 is the barrel direction. Both shaders call this
// — the lens forwards through it, the rectifier solves it.
float radial_scale(float radius, float k1, float k2) {
    float radius_squared = radius * radius;
    return 1.0 + k1 * radius_squared + k2 * radius_squared * radius_squared;
}

// Texel `i`'s centre is at (i + 0.5) / extent in normalised coordinates. The
// engine's sampler filters linearly, so a fractional source coordinate lands
// between texels rather than snapping to one.
vec2 texel_to_sampler_coordinates(vec2 texel, ivec2 extent) {
    return (texel + 0.5) / vec2(extent);
}
"""
)


def workgroups_covering(pixels: int) -> int:
    """How many tiles it takes to cover `pixels`, the last one hanging over."""
    return (pixels + WORKGROUP_TILE_SIZE - 1) // WORKGROUP_TILE_SIZE


# Sample count for the sweep below. The function it walks is a cubic-in-r^2
# with one interior maximum, so a thousand samples locate it to a fraction of
# a pixel at any resolution this app will see.
_RECOVERABLE_RADIUS_SWEEP_STEPS = 1024


def largest_recoverable_normalised_radius(k1: float, k2: float) -> float:
    """How far out the rectifier can reconstruct anything, in normalised radii.

    The forward warp is a pull: the pixel it writes at radius `r` is sampled
    from radius `r * radial_scale(r)`. Under a barrel (`k1 < 0`) that factor is
    below one, so the whole frame is sampled from an inner disc and everything
    outside that disc was never carried across. No inverse recovers it — there
    is nothing there to recover — so the rectifier masks it rather than letting
    Newton chase a root that does not exist.

    Swept rather than solved: the maximum is a root of a quartic, and a
    thousand evaluations of a three-term polynomial is both shorter to read and
    correct for coefficient pairs a closed form would need cases for.
    """
    largest = 0.0
    for step in range(_RECOVERABLE_RADIUS_SWEEP_STEPS + 1):
        radius = step / _RECOVERABLE_RADIUS_SWEEP_STEPS
        radius_squared = radius * radius
        sampled_from = radius * (
            1.0 + k1 * radius_squared + k2 * radius_squared * radius_squared
        )
        largest = max(largest, sampled_from)
    return largest
