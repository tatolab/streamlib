// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Big square pixels: the picture is diced into cells and each cell takes the
// colour at its centre.

#version 450

layout(location = 0) out vec4 painted_colour;

layout(set = 0, binding = 0) uniform sampler2D upstream_frame;

const int CELL_SIZE_IN_PIXELS = 16;

void main() {
    ivec2 fragment_pixel_coordinate = ivec2(gl_FragCoord.xy);
    ivec2 cell_origin = (fragment_pixel_coordinate / CELL_SIZE_IN_PIXELS) * CELL_SIZE_IN_PIXELS;
    // Clamped because a cell hanging off the right or bottom edge has its
    // centre outside the frame.
    ivec2 cell_centre = min(
        cell_origin + CELL_SIZE_IN_PIXELS / 2,
        textureSize(upstream_frame, 0) - 1
    );
    painted_colour = texelFetch(upstream_frame, cell_centre, 0);
}
