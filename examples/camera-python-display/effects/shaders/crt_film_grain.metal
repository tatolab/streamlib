// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#include <metal_stdlib>
using namespace metal;

// Vertex output for fullscreen quad
struct VertexOut {
    float4 position [[position]];
    float2 texCoord;
};

// Uniforms
struct CrtFilmGrainUniforms {
    float time;
    float crtCurve;           // Barrel distortion amount (0.0-1.0)
    float scanlineIntensity;  // Scanline darkness (0.0-1.0)
    float chromaticAberration; // RGB separation amount (0.0-0.01)
    float grainIntensity;     // Film grain strength (0.0-1.0)
    float grainSpeed;         // Grain animation speed
    float vignetteIntensity;  // Edge darkening (0.0-1.0)
    float brightness;         // Overall brightness multiplier
};

// Fullscreen triangle (covers screen with single triangle)
vertex VertexOut crt_vertex(uint vertexID [[vertex_id]]) {
    VertexOut out;

    float2 positions[3] = {
        float2(-1.0, -1.0),
        float2(3.0, -1.0),
        float2(-1.0, 3.0)
    };

    float2 texCoords[3] = {
        float2(0.0, 1.0),
        float2(2.0, 1.0),
        float2(0.0, -1.0)
    };

    out.position = float4(positions[vertexID], 0.0, 1.0);
    out.texCoord = texCoords[vertexID];

    return out;
}

// =============================================================================
// CRT Barrel Distortion
// =============================================================================

// Barrel distortion curve - simulates CRT screen curvature
float2 crtCurve(float2 uv, float curveAmount) {
    uv = (uv - 0.5) * 2.0;
    uv *= 1.0 + curveAmount * 0.1;
    uv.x *= 1.0 + pow(abs(uv.y) / 5.0, 2.0) * curveAmount;
    uv.y *= 1.0 + pow(abs(uv.x) / 4.0, 2.0) * curveAmount;
    uv = (uv / 2.0) + 0.5;
    uv = uv * (0.92 + 0.08 * (1.0 - curveAmount)) + (0.04 * curveAmount);
    return uv;
}

// =============================================================================
// Film Grain - 80s Blade Runner Style
// =============================================================================

// High quality hash for film grain (no visible patterns)
float hash12(float2 p) {
    float3 p3 = fract(float3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

float hash13(float3 p3) {
    p3 = fract(p3 * 0.1031);
    p3 += dot(p3, p3.zyx + 31.32);
    return fract((p3.x + p3.y) * p3.z);
}

// Film grain - static random noise that changes each frame
// Real film grain doesn't scroll - it's random speckling per frame
float filmGrain(float2 uv, float time, float speed) {
    // Quantize time to create frame-based changes (like real film frames)
    // This makes grain change randomly per-frame rather than smoothly scrolling
    float frame = floor(time * speed * 24.0); // 24fps film look

    // Fine grain - random speckling across the image
    // Use screen position + frame number for completely random per-frame noise
    float grain = hash13(float3(uv * 1000.0, frame));

    // Add slight variation at different scales for more organic look
    grain += hash13(float3(uv * 500.0 + 0.5, frame + 0.33)) * 0.5;
    grain += hash13(float3(uv * 250.0 + 0.25, frame + 0.66)) * 0.25;

    // Normalize to 0-1 range (sum of weights = 1.75)
    grain /= 1.75;

    return grain;
}

// =============================================================================
// Main Fragment Shader
// =============================================================================

fragment float4 crt_film_grain_fragment(
    VertexOut in [[stage_in]],
    texture2d<float> inputTexture [[texture(0)]],
    sampler textureSampler [[sampler(0)]],
    constant CrtFilmGrainUniforms &uniforms [[buffer(0)]]
) {
    float2 uv = in.texCoord;
    float2 originalUv = uv;

    // === CRT BARREL DISTORTION ===
    uv = crtCurve(uv, uniforms.crtCurve);

    // Check if outside curved screen bounds
    bool outsideBounds = (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0);

    // === CHROMATIC ABERRATION ===
    // RGB channel separation - classic CRT look
    float aberration = uniforms.chromaticAberration;
    float scanWobble = sin(0.3 * uniforms.time + uv.y * 21.0) *
                       sin(0.7 * uniforms.time + uv.y * 29.0) *
                       sin(0.3 + 0.33 * uniforms.time + uv.y * 31.0) * 0.0017;

    float3 col;
    col.r = inputTexture.sample(textureSampler, float2(scanWobble + uv.x + aberration, uv.y + aberration * 0.5)).r;
    col.g = inputTexture.sample(textureSampler, float2(scanWobble + uv.x, uv.y - aberration)).g;
    col.b = inputTexture.sample(textureSampler, float2(scanWobble + uv.x - aberration, uv.y + aberration * 0.3)).b;

    // Ghost/bloom from chromatic aberration
    col.r += 0.08 * inputTexture.sample(textureSampler, 0.75 * float2(scanWobble + 0.025, -0.027) + float2(uv.x + aberration, uv.y + aberration * 0.5)).r;
    col.g += 0.05 * inputTexture.sample(textureSampler, 0.75 * float2(scanWobble - 0.022, -0.02) + float2(uv.x, uv.y - aberration)).g;
    col.b += 0.08 * inputTexture.sample(textureSampler, 0.75 * float2(scanWobble - 0.02, -0.018) + float2(uv.x - aberration, uv.y + aberration * 0.3)).b;

    // === CONTRAST/COLOR PROCESSING ===
    // Slight S-curve for that analog video look
    col = clamp(col * 0.6 + 0.4 * col * col, 0.0, 1.0);

    // === VIGNETTE ===
    float vig = (0.0 + 1.0 * 16.0 * uv.x * uv.y * (1.0 - uv.x) * (1.0 - uv.y));
    vig = pow(vig, 0.3 + uniforms.vignetteIntensity * 0.4);
    col *= float3(vig);

    // Slight color tint (greenish CRT phosphor)
    col *= float3(0.95, 1.05, 0.95);

    // Brightness boost
    col *= uniforms.brightness;

    // === SCANLINES ===
    // Animated scanlines
    float scanlinePhase = 3.5 * uniforms.time + uv.y * 1080.0 * 1.5; // Assuming 1080p
    float scans = clamp(0.35 + 0.35 * sin(scanlinePhase), 0.0, 1.0);
    scans = pow(scans, 1.7);
    col = col * float3(0.4 + (1.0 - uniforms.scanlineIntensity) * 0.6 + uniforms.scanlineIntensity * 0.6 * scans);

    // Slight flicker
    col *= 1.0 + 0.01 * sin(110.0 * uniforms.time);

    // === PIXEL GRID (RGB subpixels) ===
    // Simulate RGB phosphor grid
    float pixelGrid = clamp((fmod(in.position.x, 2.0) - 1.0) * 2.0, 0.0, 1.0);
    col *= 1.0 - 0.3 * uniforms.scanlineIntensity * float3(pixelGrid);

    // === FILM GRAIN ===
    float grain = filmGrain(originalUv, uniforms.time, uniforms.grainSpeed);
    // Apply grain - affects shadows more than highlights (realistic film response)
    float luminance = dot(col, float3(0.299, 0.587, 0.114));
    float grainMask = 1.0 - luminance * 0.5; // More grain in shadows
    col += (grain - 0.5) * uniforms.grainIntensity * grainMask;

    // === OUTSIDE BOUNDS ===
    if (outsideBounds) {
        col = float3(0.0);
    }

    return float4(clamp(col, 0.0, 1.0), 1.0);
}
