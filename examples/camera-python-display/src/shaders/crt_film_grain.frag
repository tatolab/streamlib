// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// 80s Blade Runner CRT + film-grain post-effect, fragment-shader form.
// Mirrors the macOS Metal fragment shader at
// shaders/crt_film_grain.metal; ported from the pre-#487 compute shader
// (storage-buffer in/out, manual bilinear) to a fullscreen-triangle
// vertex + fragment pair using a hardware sampler.
//
// All effects are preserved verbatim: barrel curve, scanlines (resolution-
// relative — Metal had hardcoded 1080), chromatic aberration with ghost
// taps, vignette, phosphor tint, flicker, RGB pixel grid, and a 3-octave
// hash-based film grain quantized to 24 fps.

#version 450

layout(location = 0) in  vec2 inUV;
layout(location = 0) out vec4 outColor;

layout(set = 0, binding = 0) uniform sampler2D videoTex;

layout(push_constant) uniform PushConstants {
    uint  width;
    uint  height;
    float time;
    float crt_curve;
    float scanline_intensity;
    float chromatic_aberration;
    float grain_intensity;
    float grain_speed;
    float vignette_intensity;
    float brightness;
} pc;

// Barrel distortion — simulates CRT screen curvature. UV in/out 0..1.
vec2 crt_curve(vec2 uv, float curve_amount) {
    uv = (uv - 0.5) * 2.0;
    uv *= 1.0 + curve_amount * 0.1;
    uv.x *= 1.0 + pow(abs(uv.y) / 5.0, 2.0) * curve_amount;
    uv.y *= 1.0 + pow(abs(uv.x) / 4.0, 2.0) * curve_amount;
    uv = (uv / 2.0) + 0.5;
    uv = uv * (0.92 + 0.08 * (1.0 - curve_amount)) + (0.04 * curve_amount);
    return uv;
}

float hash13(vec3 p3) {
    p3 = fract(p3 * 0.1031);
    p3 += dot(p3, p3.zyx + 31.32);
    return fract((p3.x + p3.y) * p3.z);
}

// 3-octave per-frame speckle grain quantized to 24 fps — real film grain
// doesn't scroll, it re-randomizes per discrete frame. Weights sum to
// 1.75; result is normalized into 0..1.
float film_grain(vec2 uv, float time, float speed) {
    float frame = floor(time * speed * 24.0);
    float grain = hash13(vec3(uv * 1000.0, frame));
    grain += hash13(vec3(uv * 500.0  + 0.5,  frame + 0.33)) * 0.5;
    grain += hash13(vec3(uv * 250.0  + 0.25, frame + 0.66)) * 0.25;
    grain /= 1.75;
    return grain;
}

void main() {
    vec2 uv = inUV;
    vec2 original_uv = uv;

    // === CRT BARREL DISTORTION ===
    uv = crt_curve(uv, pc.crt_curve);
    bool outside_bounds = (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0);

    // === CHROMATIC ABERRATION ===
    float aberration = pc.chromatic_aberration;
    float scan_wobble = sin(0.3        * pc.time + uv.y * 21.0) *
                        sin(0.7        * pc.time + uv.y * 29.0) *
                        sin(0.3 + 0.33 * pc.time + uv.y * 31.0) * 0.0017;

    vec3 col;
    col.r = texture(videoTex, vec2(scan_wobble + uv.x + aberration,       uv.y + aberration * 0.5)).r;
    col.g = texture(videoTex, vec2(scan_wobble + uv.x,                    uv.y - aberration)).g;
    col.b = texture(videoTex, vec2(scan_wobble + uv.x - aberration,       uv.y + aberration * 0.3)).b;

    // Ghost / bloom from chromatic aberration — second tap per channel
    // weighted lightly. Offsets reproduce the Metal version verbatim.
    col.r += 0.08 * texture(videoTex, 0.75 * vec2(scan_wobble + 0.025, -0.027) + vec2(uv.x + aberration, uv.y + aberration * 0.5)).r;
    col.g += 0.05 * texture(videoTex, 0.75 * vec2(scan_wobble - 0.022, -0.02 ) + vec2(uv.x,              uv.y - aberration)).g;
    col.b += 0.08 * texture(videoTex, 0.75 * vec2(scan_wobble - 0.02,  -0.018) + vec2(uv.x - aberration, uv.y + aberration * 0.3)).b;

    // === S-CURVE CONTRAST ===
    col = clamp(col * 0.6 + 0.4 * col * col, 0.0, 1.0);

    // === VIGNETTE ===
    float vig = 16.0 * uv.x * uv.y * (1.0 - uv.x) * (1.0 - uv.y);
    vig = pow(max(vig, 0.0), 0.3 + pc.vignette_intensity * 0.4);
    col *= vig;

    // === PHOSPHOR TINT ===
    col *= vec3(0.95, 1.05, 0.95);

    // === BRIGHTNESS ===
    col *= pc.brightness;

    // === SCANLINES ===
    // Phase factor was hardcoded to 1080 in the Metal version (assuming
    // 1080p); using `pc.height` keeps the cycles-per-pixel ratio
    // resolution-independent — at 1080p the phase is identical.
    float scanline_phase = 3.5 * pc.time + uv.y * float(pc.height) * 1.5;
    float scans = clamp(0.35 + 0.35 * sin(scanline_phase), 0.0, 1.0);
    scans = pow(scans, 1.7);
    col *= 0.4 + (1.0 - pc.scanline_intensity) * 0.6 + pc.scanline_intensity * 0.6 * scans;

    // === FLICKER ===
    col *= 1.0 + 0.01 * sin(110.0 * pc.time);

    // === RGB PIXEL GRID ===
    // Compute pixel x from gl_FragCoord — equivalent to the .comp's
    // `gl_GlobalInvocationID.x` and the Metal version's `in.position.x`.
    float pixel_grid = clamp((mod(gl_FragCoord.x, 2.0) - 1.0) * 2.0, 0.0, 1.0);
    col *= 1.0 - 0.3 * pc.scanline_intensity * pixel_grid;

    // === FILM GRAIN ===
    float grain = film_grain(original_uv, pc.time, pc.grain_speed);
    float luminance = dot(col, vec3(0.299, 0.587, 0.114));
    float grain_mask = 1.0 - luminance * 0.5;
    col += (grain - 0.5) * pc.grain_intensity * grain_mask;

    // === OUTSIDE BOUNDS ===
    if (outside_bounds) {
        col = vec3(0.0);
    }

    outColor = vec4(clamp(col, 0.0, 1.0), 1.0);
}
