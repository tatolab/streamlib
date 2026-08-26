// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Intermittent glitch flash stacked under a continuous cyberpunk grade.
//
// Pass 1 fires only while the processor's state machine says a glitch is
// running: chromatic aberration and sparse slice displacement in the subtle
// mode, a wide horizontal slice tear plus multi-octave grain in the dramatic
// one. Pass 2 always runs — the teal/magenta cross-channel grade, a light
// scanline and a vignette — so the picture carries the look between flashes.

#version 450

layout(location = 0) in vec2 screen_uv;
layout(location = 0) out vec4 graded_colour;

layout(set = 0, binding = 0) uniform sampler2D video_from_upstream;

layout(push_constant, std430) uniform CyberpunkGlitchPushConstants {
    vec2 frame_extent_in_pixels;
    float elapsed_seconds;
    float glitch_intensity;
    float glitch_seed;
    // 1.0 while a dramatic glitch is running, 0.0 for the subtle variant.
    float dramatic_glitch;
} pc;

float hash11(float p) {
    p = fract(p * 0.1031);
    p *= p + 33.33;
    p *= p + p;
    return fract(p);
}

float hash21(vec2 p) {
    p = fract(p * vec2(234.34, 435.345));
    p += dot(p, p + 34.23);
    return fract(p.x * p.y);
}

vec3 dramatic_glitch_colour(vec2 uv, vec2 inverse_extent) {
    float slice_height_in_pixels = mix(15.0, 30.0, hash11(pc.glitch_seed * 0.5));
    float slice_index = floor(uv.y * pc.frame_extent_in_pixels.y / slice_height_in_pixels);
    float slice_random = hash11(slice_index + pc.glitch_seed);
    float x_offset_in_pixels = (slice_random - 0.5) * 2.0 * (200.0 * pc.glitch_intensity);
    vec3 colour = texture(video_from_upstream, uv + vec2(x_offset_in_pixels * inverse_extent.x, 0.0)).rgb;

    float grain = 0.0;
    float scale = 1.0;
    float amplitude = 0.5;
    for (int octave = 0; octave < 3; octave++) {
        grain += (hash21(uv * pc.frame_extent_in_pixels * scale * 0.01 + pc.glitch_seed) - 0.5) * amplitude;
        scale *= 2.0;
        amplitude *= 0.5;
    }
    grain += (hash21(uv * 800.0 + pc.elapsed_seconds * 10.0) - 0.5) * 0.15;
    return clamp(colour + grain * 0.12 * pc.glitch_intensity, 0.0, 1.0);
}

vec3 subtle_glitch_colour(vec2 uv, vec2 inverse_extent) {
    float aberration_in_pixels = 8.0 * pc.glitch_intensity;
    vec2 red_offset = vec2(aberration_in_pixels * (hash11(pc.glitch_seed * 1.1) - 0.5) * 2.0 * inverse_extent.x, 0.0);
    vec2 blue_offset = vec2(aberration_in_pixels * (hash11(pc.glitch_seed * 2.2) - 0.5) * 2.0 * inverse_extent.x, 0.0);
    vec3 colour = vec3(
        texture(video_from_upstream, uv + red_offset).r,
        texture(video_from_upstream, uv).g,
        texture(video_from_upstream, uv + blue_offset).b
    );

    float slice_noise = hash11(floor(uv.y * 60.0) + pc.glitch_seed);
    if (slice_noise > 0.75 && pc.glitch_intensity > 0.3) {
        float slice_strength = (slice_noise - 0.75) / 0.25;
        float slice_offset_in_pixels =
            (hash11(pc.glitch_seed + floor(uv.y * 60.0) * 0.1) - 0.5)
            * 60.0 * pc.glitch_intensity * slice_strength;
        colour = texture(video_from_upstream, uv + vec2(slice_offset_in_pixels * inverse_extent.x, 0.0)).rgb;
    }

    float line_noise = hash11(pc.elapsed_seconds * 50.0 + floor(uv.y * pc.frame_extent_in_pixels.y));
    if (line_noise > 0.97) {
        colour += vec3(0.0, 0.3 * pc.glitch_intensity, 0.3 * pc.glitch_intensity);
    }
    return colour;
}

void main() {
    vec2 uv = screen_uv;
    vec2 inverse_extent = 1.0 / pc.frame_extent_in_pixels;

    vec3 colour;
    if (pc.glitch_intensity < 0.01) {
        colour = texture(video_from_upstream, uv).rgb;
    } else if (pc.dramatic_glitch > 0.5) {
        colour = dramatic_glitch_colour(uv, inverse_extent);
    } else {
        colour = subtle_glitch_colour(uv, inverse_extent);
    }

    // Slight boost to reds and blues with green left flat to hold skin tones,
    // plus an R↔B cross-channel bleed — the teal/magenta cyberpunk feel.
    vec3 graded;
    graded.r = colour.r * 1.08 + colour.g * 0.02 + colour.b * 0.03;
    graded.g = colour.g;
    graded.b = colour.r * 0.03 + colour.g * 0.02 + colour.b * 1.06;

    graded = mix(graded, graded * vec3(0.78, 1.05, 1.18), 0.55);
    graded = mix(graded, graded + vec3(0.04, 0.0, 0.06), 0.35);

    // Kept light on purpose: the CRT pass downstream lays the heavy scanlines
    // and the film grain over the top of this.
    graded *= 0.94 + 0.06 * sin(uv.y * 800.0);

    vec2 uv_from_centre = uv * 2.0 - 1.0;
    graded *= clamp(1.0 - dot(uv_from_centre, uv_from_centre) * 0.35, 0.5, 1.0);

    graded_colour = vec4(graded, 1.0);
}
