// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#include <metal_stdlib>
using namespace metal;

struct VertexOut {
    float4 position [[position]];
    float2 texCoords;
};

struct CompositorUniforms {
    float time;
    float maskThreshold;
    float edgeFeather;  // Width of edge feathering (0.0-0.5)
    float padding;
};

// Hash function for procedural noise
float hash21(float2 p) {
    p = fract(p * float2(234.34, 435.345));
    p += dot(p, p + 34.23);
    return fract(p.x * p.y);
}

// Smooth noise
float noise(float2 p) {
    float2 i = floor(p);
    float2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    float a = hash21(i);
    float b = hash21(i + float2(1.0, 0.0));
    float c = hash21(i + float2(0.0, 1.0));
    float d = hash21(i + float2(1.0, 1.0));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

// Fractal Brownian Motion
float fbm(float2 p) {
    float value = 0.0;
    float amplitude = 0.5;
    for (int i = 0; i < 4; i++) {
        value += amplitude * noise(p);
        p *= 2.0;
        amplitude *= 0.5;
    }
    return value;
}

// Generate cyberpunk city background
float3 cyberpunkBackground(float2 uv, float time) {
    float3 color = float3(0.02, 0.02, 0.05);

    // Neon grid on the ground
    if (uv.y > 0.6) {
        float gridY = (uv.y - 0.6) / 0.4;
        float perspective = 1.0 / (gridY + 0.1);
        float hLine = abs(sin(gridY * 30.0 * perspective)) * perspective;
        hLine = smoothstep(0.95, 1.0, hLine);
        float vLine = abs(sin((uv.x - 0.5) * 40.0 * perspective + time * 0.5));
        vLine = smoothstep(0.97, 1.0, vLine) * (1.0 - gridY);
        color += float3(0.0, 0.8, 1.0) * hLine * 0.3;
        color += float3(1.0, 0.0, 0.8) * vLine * 0.2;
    }

    // City silhouette
    float cityLine = 0.55 + 0.1 * fbm(float2(uv.x * 8.0, 0.0));
    if (uv.y > cityLine && uv.y < cityLine + 0.15) {
        float buildingNoise = fbm(float2(uv.x * 20.0, 0.0));
        float buildingHeight = cityLine + buildingNoise * 0.12;
        if (uv.y < buildingHeight) {
            color = float3(0.01, 0.01, 0.02);
            float2 windowCoord = float2(uv.x * 100.0, (uv.y - cityLine) * 80.0);
            float windowNoise = hash21(floor(windowCoord));
            if (windowNoise > 0.7) {
                float windowColor = hash21(floor(windowCoord) + 100.0);
                if (windowColor < 0.33) {
                    color = float3(0.0, 0.8, 1.0) * 0.5;
                } else if (windowColor < 0.66) {
                    color = float3(1.0, 0.9, 0.2) * 0.4;
                } else {
                    color = float3(1.0, 0.0, 0.8) * 0.4;
                }
            }
        }
    }

    // Sky gradient
    if (uv.y < 0.6) {
        float skyGrad = uv.y / 0.6;
        color = mix(float3(0.1, 0.0, 0.15), float3(0.02, 0.02, 0.08), skyGrad);
        float clouds = fbm(float2(uv.x * 3.0 + time * 0.05, uv.y * 2.0));
        clouds = smoothstep(0.4, 0.6, clouds);
        color += float3(0.8, 0.0, 0.5) * clouds * 0.15 * (1.0 - skyGrad);
        float stars = hash21(floor(uv * 200.0));
        if (stars > 0.995 && skyGrad < 0.5) {
            color += float3(1.0) * (1.0 - skyGrad * 2.0) * 0.5;
        }
    }

    // Scanlines
    float scanLine = fract(uv.y * 200.0 + time * 2.0);
    scanLine = smoothstep(0.0, 0.1, scanLine) * smoothstep(0.2, 0.1, scanLine);
    color *= 1.0 - scanLine * 0.05;

    return color;
}

// Cyberpunk color grading
float3 cyberpunkColorGrade(float3 color, float redBoost, float blueBoost) {
    float3 graded;
    graded.r = color.r * redBoost + color.g * 0.02 + color.b * 0.03;
    graded.g = color.g;
    graded.b = color.r * 0.03 + color.g * 0.02 + color.b * blueBoost;
    return clamp(graded, 0.0, 1.0);
}

vertex VertexOut compositor_vertex(uint vertexID [[vertex_id]]) {
    float x = float((vertexID & 1) << 2) - 1.0;
    float y = float((vertexID & 2) << 1) - 1.0;
    VertexOut out;
    out.position = float4(x, y, 0.0, 1.0);
    out.texCoords = float2((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// With background image texture
fragment float4 compositor_fragment(
    VertexOut in [[stage_in]],
    texture2d<float> videoTexture [[texture(0)]],
    texture2d<float> maskTexture [[texture(1)]],
    texture2d<float> backgroundTexture [[texture(2)]],
    sampler texSampler [[sampler(0)]],
    constant CompositorUniforms &uniforms [[buffer(0)]]
) {
    float2 uv = in.texCoords;
    float4 videoColor = videoTexture.sample(texSampler, uv);
    float mask = maskTexture.sample(texSampler, uv).r;

    // Use configurable edge feathering for smoother transitions
    float feather = uniforms.edgeFeather;
    float alpha = smoothstep(uniforms.maskThreshold - feather, uniforms.maskThreshold + feather, mask);

    // Sample background image - swap R/B due to CoreGraphics BGRA format
    float3 background = backgroundTexture.sample(texSampler, uv).bgr;

    // Video texture colors - use as-is (pipeline handles format correctly)
    float3 person = videoColor.rgb;
    float3 result = mix(background, person, alpha);
    return float4(result, 1.0);
}

// Procedural background (no image)
fragment float4 compositor_procedural_fragment(
    VertexOut in [[stage_in]],
    texture2d<float> videoTexture [[texture(0)]],
    texture2d<float> maskTexture [[texture(1)]],
    sampler texSampler [[sampler(0)]],
    constant CompositorUniforms &uniforms [[buffer(0)]]
) {
    float2 uv = in.texCoords;
    float4 videoColor = videoTexture.sample(texSampler, uv);
    float mask = maskTexture.sample(texSampler, uv).r;

    // Use configurable edge feathering for smoother transitions
    float feather = uniforms.edgeFeather;
    float alpha = smoothstep(uniforms.maskThreshold - feather, uniforms.maskThreshold + feather, mask);

    // Use procedural cyberpunk background
    float3 background = cyberpunkBackground(uv, uniforms.time);

    // Video texture colors - use as-is (pipeline handles format correctly)
    float3 person = videoColor.rgb;
    float3 result = mix(background, person, alpha);
    return float4(result, 1.0);
}

// Passthrough only (no segmentation available)
fragment float4 colorgrade_only_fragment(
    VertexOut in [[stage_in]],
    texture2d<float> videoTexture [[texture(0)]],
    sampler texSampler [[sampler(0)]],
    constant CompositorUniforms &uniforms [[buffer(0)]]
) {
    // When no mask is available, pass through the video with color swap fix.
    // Video texture is labeled RGBA but has BGRA bytes, so swap R and B.
    float4 color = videoTexture.sample(texSampler, in.texCoords);
    return float4(color.b, color.g, color.r, color.a);
}

// =============================================================================
// RGBA → BGRA Compute Kernel (for Vision/CoreVideo compatibility)
// =============================================================================

kernel void rgba_to_bgra(
    texture2d<float, access::read> inputTexture [[texture(0)]],
    texture2d<float, access::write> outputTexture [[texture(1)]],
    uint2 gid [[thread_position_in_grid]]
) {
    if (gid.x >= inputTexture.get_width() || gid.y >= inputTexture.get_height()) {
        return;
    }

    float4 rgba = inputTexture.read(gid);
    // Swizzle RGBA → BGRA
    float4 bgra = float4(rgba.b, rgba.g, rgba.r, rgba.a);
    outputTexture.write(bgra, gid);
}

// =============================================================================
// Temporal Blending (EMA) Compute Kernel
// =============================================================================
// Blends current mask with previous frame's mask using exponential moving average.
// This reduces frame-to-frame jitter in the segmentation mask.
// Formula: output = (1 - blendFactor) * current + blendFactor * previous

struct TemporalBlendParams {
    float blendFactor;  // How much of the previous frame to keep (0.0-1.0)
    float3 padding;
};

kernel void temporal_blend_mask(
    texture2d<float, access::read> currentMask [[texture(0)]],
    texture2d<float, access::read> previousMask [[texture(1)]],
    texture2d<float, access::write> outputMask [[texture(2)]],
    constant TemporalBlendParams &params [[buffer(0)]],
    uint2 gid [[thread_position_in_grid]]
) {
    if (gid.x >= currentMask.get_width() || gid.y >= currentMask.get_height()) {
        return;
    }

    float current = currentMask.read(gid).r;
    float previous = previousMask.read(gid).r;

    // Exponential moving average
    float blended = mix(current, previous, params.blendFactor);

    outputMask.write(float4(blended, 0.0, 0.0, 1.0), gid);
}

// =============================================================================
// Separable Gaussian Blur Compute Kernels
// =============================================================================
// Two-pass separable Gaussian blur for efficient mask edge smoothing.
// Horizontal pass followed by vertical pass.

// Gaussian kernel weights for sigma ~3.0 (9-tap kernel)
// Pre-computed: exp(-x^2 / (2 * sigma^2)) normalized
constant float gaussianWeights[9] = {
    0.028532, 0.067234, 0.124009, 0.179044, 0.202360,
    0.179044, 0.124009, 0.067234, 0.028532
};

kernel void gaussian_blur_horizontal(
    texture2d<float, access::read> inputMask [[texture(0)]],
    texture2d<float, access::write> outputMask [[texture(1)]],
    uint2 gid [[thread_position_in_grid]]
) {
    uint width = inputMask.get_width();
    uint height = inputMask.get_height();

    if (gid.x >= width || gid.y >= height) {
        return;
    }

    float sum = 0.0;

    for (int i = -4; i <= 4; i++) {
        int x = clamp(int(gid.x) + i, 0, int(width) - 1);
        float sample = inputMask.read(uint2(x, gid.y)).r;
        sum += sample * gaussianWeights[i + 4];
    }

    outputMask.write(float4(sum, 0.0, 0.0, 1.0), gid);
}

kernel void gaussian_blur_vertical(
    texture2d<float, access::read> inputMask [[texture(0)]],
    texture2d<float, access::write> outputMask [[texture(1)]],
    uint2 gid [[thread_position_in_grid]]
) {
    uint width = inputMask.get_width();
    uint height = inputMask.get_height();

    if (gid.x >= width || gid.y >= height) {
        return;
    }

    float sum = 0.0;

    for (int i = -4; i <= 4; i++) {
        int y = clamp(int(gid.y) + i, 0, int(height) - 1);
        float sample = inputMask.read(uint2(gid.x, y)).r;
        sum += sample * gaussianWeights[i + 4];
    }

    outputMask.write(float4(sum, 0.0, 0.0, 1.0), gid);
}
