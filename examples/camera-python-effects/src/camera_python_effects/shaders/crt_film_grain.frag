// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// 80s CRT + film-grain post-effect: barrel curve, chromatic aberration with
// ghost taps, S-curve contrast, vignette, phosphor tint, scanlines, flicker,
// RGB pixel grid, and a 3-octave hash grain quantized to 24 fps.

#version 450

layout(location = 0) in vec2 screen_uv;
layout(location = 0) out vec4 crt_colour;

layout(set = 0, binding = 0) uniform sampler2D video_from_upstream;

layout(push_constant, std430) uniform CrtFilmGrainPushConstants {
    vec2 frame_extent_in_pixels;
    float elapsed_seconds;
    float barrel_curve;
    float scanline_intensity;
    float chromatic_aberration;
    float grain_intensity;
    float grain_speed;
    float vignette_intensity;
    float brightness;
} pc;

// Simulates the curvature of a CRT screen. UV in and out are both 0..1.
vec2 curve_like_a_crt(vec2 uv, float curve_amount) {
    uv = (uv - 0.5) * 2.0;
    uv *= 1.0 + curve_amount * 0.1;
    uv.x *= 1.0 + pow(abs(uv.y) / 5.0, 2.0) * curve_amount;
    uv.y *= 1.0 + pow(abs(uv.x) / 4.0, 2.0) * curve_amount;
    uv = (uv / 2.0) + 0.5;
    return uv * (0.92 + 0.08 * (1.0 - curve_amount)) + (0.04 * curve_amount);
}

float hash13(vec3 p3) {
    p3 = fract(p3 * 0.1031);
    p3 += dot(p3, p3.zyx + 31.32);
    return fract((p3.x + p3.y) * p3.z);
}

// Real film grain does not scroll, it re-randomizes per discrete frame — so
// the octaves are seeded off a 24 fps frame index rather than continuous time.
// The three weights sum to 1.75, which is what normalizes the result to 0..1.
float film_grain(vec2 uv, float elapsed_seconds, float speed) {
    float frame = floor(elapsed_seconds * speed * 24.0);
    float grain = hash13(vec3(uv * 1000.0, frame));
    grain += hash13(vec3(uv * 500.0 + 0.5, frame + 0.33)) * 0.5;
    grain += hash13(vec3(uv * 250.0 + 0.25, frame + 0.66)) * 0.25;
    return grain / 1.75;
}

void main() {
    vec2 original_uv = screen_uv;
    vec2 uv = curve_like_a_crt(screen_uv, pc.barrel_curve);
    bool outside_the_tube = (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0);

    float aberration = pc.chromatic_aberration;
    float scan_wobble = sin(0.3 * pc.elapsed_seconds + uv.y * 21.0)
                      * sin(0.7 * pc.elapsed_seconds + uv.y * 29.0)
                      * sin(0.3 + 0.33 * pc.elapsed_seconds + uv.y * 31.0) * 0.0017;

    vec3 colour;
    colour.r = texture(video_from_upstream, vec2(scan_wobble + uv.x + aberration, uv.y + aberration * 0.5)).r;
    colour.g = texture(video_from_upstream, vec2(scan_wobble + uv.x, uv.y - aberration)).g;
    colour.b = texture(video_from_upstream, vec2(scan_wobble + uv.x - aberration, uv.y + aberration * 0.3)).b;

    // A lightly-weighted second tap per channel — the bloom that sells the
    // aberration as a phosphor smear rather than a channel offset.
    colour.r += 0.08 * texture(video_from_upstream, 0.75 * vec2(scan_wobble + 0.025, -0.027) + vec2(uv.x + aberration, uv.y + aberration * 0.5)).r;
    colour.g += 0.05 * texture(video_from_upstream, 0.75 * vec2(scan_wobble - 0.022, -0.020) + vec2(uv.x, uv.y - aberration)).g;
    colour.b += 0.08 * texture(video_from_upstream, 0.75 * vec2(scan_wobble - 0.020, -0.018) + vec2(uv.x - aberration, uv.y + aberration * 0.3)).b;

    colour = clamp(colour * 0.6 + 0.4 * colour * colour, 0.0, 1.0);

    float vignette = 16.0 * uv.x * uv.y * (1.0 - uv.x) * (1.0 - uv.y);
    colour *= pow(max(vignette, 0.0), 0.3 + pc.vignette_intensity * 0.4);

    colour *= vec3(0.95, 1.05, 0.95);
    colour *= pc.brightness;

    // Phase scales with the frame height so the cycles-per-pixel ratio is
    // resolution-independent.
    float scanline_phase = 3.5 * pc.elapsed_seconds + uv.y * pc.frame_extent_in_pixels.y * 1.5;
    float scanlines = pow(clamp(0.35 + 0.35 * sin(scanline_phase), 0.0, 1.0), 1.7);
    colour *= 0.4 + (1.0 - pc.scanline_intensity) * 0.6 + pc.scanline_intensity * 0.6 * scanlines;

    colour *= 1.0 + 0.01 * sin(110.0 * pc.elapsed_seconds);

    float pixel_grid = clamp((mod(gl_FragCoord.x, 2.0) - 1.0) * 2.0, 0.0, 1.0);
    colour *= 1.0 - 0.3 * pc.scanline_intensity * pixel_grid;

    float grain = film_grain(original_uv, pc.elapsed_seconds, pc.grain_speed);
    float luminance = dot(colour, vec3(0.299, 0.587, 0.114));
    colour += (grain - 0.5) * pc.grain_intensity * (1.0 - luminance * 0.5);

    if (outside_the_tube) {
        colour = vec3(0.0);
    }

    crt_colour = vec4(clamp(colour, 0.0, 1.0), 1.0);
}
