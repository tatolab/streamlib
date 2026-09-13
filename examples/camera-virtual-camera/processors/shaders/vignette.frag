// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// A lens vignette: the middle of the picture untouched, the corners fading
// towards black.

#version 450

layout(location = 0) in vec2 screen_uv;
layout(location = 0) out vec4 painted_colour;

layout(set = 0, binding = 0) uniform sampler2D upstream_frame;

// Distances from the centre, in the frame's own 0..1 coordinates: nothing
// darkens inside the first, everything past the second is black. A corner sits
// at about 0.707, so it keeps a little of its picture.
const float FADE_STARTS_AT = 0.35;
const float FADE_ENDS_AT = 0.8;

void main() {
    vec4 source = texture(upstream_frame, screen_uv);
    float distance_from_centre = distance(screen_uv, vec2(0.5));
    float light_kept = 1.0 - smoothstep(FADE_STARTS_AT, FADE_ENDS_AT, distance_from_centre);
    painted_colour = vec4(source.rgb * light_kept, source.a);
}
