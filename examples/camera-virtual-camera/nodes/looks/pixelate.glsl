// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Big square pixels: the picture is diced into cells and each cell takes the
// colour at its centre.

const int CELL_SIZE_IN_PIXELS = 16;

vec4 effect(vec4 source, ivec2 at) {
    ivec2 cell_origin = (at / CELL_SIZE_IN_PIXELS) * CELL_SIZE_IN_PIXELS;
    // `streamlib_source_at` clamps to the frame's edge, which a cell hanging
    // off the right or bottom has its centre past.
    return streamlib_source_at(cell_origin + CELL_SIZE_IN_PIXELS / 2);
}
