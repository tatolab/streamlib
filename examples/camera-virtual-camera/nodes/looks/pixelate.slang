// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Big square pixels: the picture is diced into cells and each cell takes the
// colour at its centre.

static const int CELL_SIZE_IN_PIXELS = 16;

float4 effect(float4 source, int2 at) {
    int2 cell_origin = (at / CELL_SIZE_IN_PIXELS) * CELL_SIZE_IN_PIXELS;
    // `shader_effect_source_at` clamps to the frame's edge, which a cell
    // hanging off the right or bottom has its centre past.
    return shader_effect_source_at(cell_origin + CELL_SIZE_IN_PIXELS / 2);
}
