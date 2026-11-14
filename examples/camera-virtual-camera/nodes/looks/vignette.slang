// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// A lens vignette: the middle of the picture untouched, the corners fading
// towards black.

// Distances from the centre, in the frame's own 0..1 coordinates: nothing
// darkens inside the first, everything past the second is black. A corner sits
// at about 0.707, so it keeps a little of its picture.
static const float FADE_STARTS_AT = 0.35;
static const float FADE_ENDS_AT = 0.8;

float4 effect(float4 source, int2 at) {
    float2 frame_uv = (float2(at) + 0.5) / float2(shader_effect_frame_extent());
    float distance_from_centre = distance(frame_uv, float2(0.5));
    float light_kept = 1.0 - smoothstep(FADE_STARTS_AT, FADE_ENDS_AT, distance_from_centre);
    return float4(source.rgb * light_kept, source.a);
}
