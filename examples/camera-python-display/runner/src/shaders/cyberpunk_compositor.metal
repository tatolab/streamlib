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

// =============================================================================
// Color Space Conversions
// =============================================================================

float3 rgb_to_hsl(float3 rgb) {
    float maxC = max(max(rgb.r, rgb.g), rgb.b);
    float minC = min(min(rgb.r, rgb.g), rgb.b);
    float delta = maxC - minC;

    float h = 0.0;
    float s = 0.0;
    float l = (maxC + minC) * 0.5;

    if (delta > 0.0001) {
        s = (l < 0.5) ? (delta / (maxC + minC)) : (delta / (2.0 - maxC - minC));

        if (maxC == rgb.r) {
            h = (rgb.g - rgb.b) / delta + (rgb.g < rgb.b ? 6.0 : 0.0);
        } else if (maxC == rgb.g) {
            h = (rgb.b - rgb.r) / delta + 2.0;
        } else {
            h = (rgb.r - rgb.g) / delta + 4.0;
        }
        h /= 6.0;
    }

    return float3(h, s, l);
}

float hue_to_rgb(float p, float q, float t) {
    if (t < 0.0) t += 1.0;
    if (t > 1.0) t -= 1.0;
    if (t < 1.0/6.0) return p + (q - p) * 6.0 * t;
    if (t < 1.0/2.0) return q;
    if (t < 2.0/3.0) return p + (q - p) * (2.0/3.0 - t) * 6.0;
    return p;
}

float3 hsl_to_rgb(float3 hsl) {
    float h = hsl.x;
    float s = hsl.y;
    float l = hsl.z;

    if (s < 0.0001) {
        return float3(l);
    }

    float q = (l < 0.5) ? (l * (1.0 + s)) : (l + s - l * s);
    float p = 2.0 * l - q;

    return float3(
        hue_to_rgb(p, q, h + 1.0/3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0/3.0)
    );
}

// =============================================================================
// Color Grading Functions
// =============================================================================

// Lift/Gamma/Gain (color wheels) - industry standard color correction
float3 apply_lift_gamma_gain(float3 color, float3 lift, float3 gamma, float3 gain) {
    // Lift affects shadows
    color = color + lift * (1.0 - color);
    // Gain affects highlights
    color = color * gain;
    // Gamma affects midtones
    color = pow(max(color, 0.0001), 1.0 / gamma);
    return color;
}

// Split toning - different color tints for shadows vs highlights
float3 apply_split_toning(float3 color, float3 shadowTint, float3 highlightTint, float balance) {
    float luma = dot(color, float3(0.2126, 0.7152, 0.0722));

    // Smooth transition between shadows and highlights
    float shadowMask = smoothstep(0.5 + balance * 0.5, 0.0, luma);
    float highlightMask = smoothstep(0.5 - balance * 0.5, 1.0, luma);

    // Apply tints
    color = mix(color, color * shadowTint, shadowMask * 0.4);
    color = mix(color, color + highlightTint * 0.15, highlightMask);

    return color;
}

// S-curve contrast enhancement
float3 apply_contrast(float3 color, float contrast) {
    // S-curve using smoothstep for natural look
    float midpoint = 0.5;
    color = mix(float3(midpoint), color, 1.0 + contrast);
    return clamp(color, 0.0, 1.0);
}

// Vibrance - intelligent saturation that protects skin tones
float3 apply_vibrance(float3 color, float vibrance) {
    float luma = dot(color, float3(0.2126, 0.7152, 0.0722));
    float maxC = max(max(color.r, color.g), color.b);
    float minC = min(min(color.r, color.g), color.b);
    float sat = (maxC > 0.0001) ? (maxC - minC) / maxC : 0.0;

    // Less saturated colors get boosted more (protects already-saturated skin)
    float boost = (1.0 - sat) * vibrance;
    return mix(float3(luma), color, 1.0 + boost);
}

// HSL-based color manipulation following Lightroom cyberpunk workflow
float3 apply_hsl_adjustments(float3 color) {
    float3 hsl = rgb_to_hsl(color);
    float h = hsl.x;
    float s = hsl.y;
    float l = hsl.z;

    // Helper to calculate hue proximity (handles wraparound)
    // Returns 0-1 where 1 = exact match, 0 = far away
    #define HUE_PROXIMITY(target, width) max(0.0, 1.0 - min(abs(h - (target)), min(abs(h - (target) + 1.0), abs(h - (target) - 1.0))) / (width))

    // === HUE SHIFTS (following Lightroom HSL advice) ===

    // Reds (h ≈ 0.0) → shift towards Pink/Magenta (increase hue)
    float redProx = HUE_PROXIMITY(0.0, 0.08);
    h = h + redProx * 0.04; // Subtle shift towards pink

    // Oranges (h ≈ 0.08) → shift towards pink/magenta
    float orangeProx = HUE_PROXIMITY(0.08, 0.06);
    h = h + orangeProx * 0.03;

    // Yellows (h ≈ 0.15) → shift slightly towards orange/pink
    float yellowProx = HUE_PROXIMITY(0.15, 0.06);
    h = h + yellowProx * 0.02;

    // Greens (h ≈ 0.33) → shift towards Cyan/Teal (increase hue)
    float greenProx = HUE_PROXIMITY(0.33, 0.1);
    h = h + greenProx * 0.06; // Shift greens to teal

    // === SATURATION ADJUSTMENTS ===

    // Boost saturation in blues (h ≈ 0.6)
    float blueProx = HUE_PROXIMITY(0.6, 0.1);
    s = mix(s, min(s * 1.2, 1.0), blueProx * 0.5);

    // Boost saturation in cyans (h ≈ 0.5)
    float cyanProx = HUE_PROXIMITY(0.5, 0.08);
    s = mix(s, min(s * 1.25, 1.0), cyanProx * 0.5);

    // Boost saturation in magentas/pinks (h ≈ 0.9)
    float magentaProx = HUE_PROXIMITY(0.9, 0.1);
    s = mix(s, min(s * 1.2, 1.0), magentaProx * 0.4);

    // Slightly reduce saturation in oranges/yellows to protect skin tones
    float skinProx = max(orangeProx, yellowProx);
    s = mix(s, s * 0.9, skinProx * 0.3);

    // Wrap hue back to 0-1 range
    h = fract(h);

    return hsl_to_rgb(float3(h, s, l));
}

// White balance / color temperature
float3 apply_color_temperature(float3 color, float temperature, float tint) {
    // Temperature: negative = cool (blue), positive = warm (orange)
    // Tint: negative = green, positive = magenta

    // Simple but effective color temperature adjustment
    color.r += temperature * 0.1;
    color.b -= temperature * 0.1;
    color.g -= tint * 0.05;
    color.r += tint * 0.025;
    color.b += tint * 0.025;

    return clamp(color, 0.0, 1.0);
}

// =============================================================================
// Edge Effects
// =============================================================================

// Subtle neon edge glow from segmentation mask
float3 apply_edge_glow(float3 color, float mask, float edgeMask, float time) {
    // Gentle animated glow - mostly cyan with hints of pink
    float hue = 0.52 + sin(time * 0.3) * 0.08; // Subtle oscillation around cyan
    float3 glowColor = hsl_to_rgb(float3(hue, 0.8, 0.55));

    // Apply glow subtly - don't overpower the image
    float glowIntensity = edgeMask * 0.4; // Reduced from 0.8
    color = color + glowColor * glowIntensity * 0.5;

    return color;
}

// Three-way color grading (Shadows / Midtones / Highlights)
// More refined than simple split toning
float3 apply_three_way_color_grade(float3 color, float3 shadowColor, float3 midtoneColor, float3 highlightColor) {
    float luma = dot(color, float3(0.2126, 0.7152, 0.0722));

    // Smooth masks for shadows, midtones, highlights
    float shadowMask = 1.0 - smoothstep(0.0, 0.4, luma);
    float highlightMask = smoothstep(0.6, 1.0, luma);
    float midtoneMask = 1.0 - shadowMask - highlightMask;
    midtoneMask = max(midtoneMask, 0.0);

    // Apply color shifts (additive for subtle effect)
    color = color + shadowColor * shadowMask * 0.08;
    color = color + midtoneColor * midtoneMask * 0.05;
    color = color + highlightColor * highlightMask * 0.06;

    return color;
}

// S-curve for tone contrast (more filmic than simple contrast)
float3 apply_s_curve(float3 color, float strength) {
    // Attempt to create a nice Bezier s-curve
    // Darken shadows, lift highlights slightly
    float3 result;
    result.r = color.r - strength * sin(color.r * 3.14159) * 0.1;
    result.g = color.g - strength * sin(color.g * 3.14159) * 0.1;
    result.b = color.b - strength * sin(color.b * 3.14159) * 0.1;
    return clamp(result, 0.0, 1.0);
}

// =============================================================================
// Main Cyberpunk Color Grading (Refined, Lightroom-style)
// =============================================================================

float3 cyberpunk_color_grade(float3 color, float time) {
    // 1. Subtle cool white balance - don't overdo it
    color = apply_color_temperature(color, -0.15, 0.05); // Gentle cool with tiny magenta

    // 2. HSL adjustments (hue shifts + selective saturation)
    //    - Reds → Pink, Greens → Teal
    //    - Boost blues/cyans/magentas, protect skin tones
    color = apply_hsl_adjustments(color);

    // 3. Three-way color grading (following Lightroom advice):
    //    - Shadows: Deep purple/blue
    //    - Midtones: Subtle pink/magenta
    //    - Highlights: Aqua/teal/green
    float3 shadowColor = float3(0.1, 0.0, 0.2);    // Deep purple
    float3 midtoneColor = float3(0.15, 0.0, 0.1);  // Subtle pink
    float3 highlightColor = float3(-0.05, 0.1, 0.1); // Aqua/teal
    color = apply_three_way_color_grade(color, shadowColor, midtoneColor, highlightColor);

    // 4. Gentle S-curve for contrast (not too dramatic)
    color = apply_s_curve(color, 0.4);

    // 5. Very subtle vibrance (protect skin tones)
    color = apply_vibrance(color, 0.15);

    // 6. Subtle lift in shadows (like Lightroom's blacks slider)
    //    Prevents pure black, gives that filmic look
    color = max(color, 0.02);

    return clamp(color, 0.0, 1.0);
}

// Hash function for procedural effects
float hash21(float2 p) {
    p = fract(p * float2(234.34, 435.345));
    p += dot(p, p + 34.23);
    return fract(p.x * p.y);
}

// =============================================================================
// Vertex Shader
// =============================================================================

vertex VertexOut compositor_vertex(uint vertexID [[vertex_id]]) {
    float x = float((vertexID & 1) << 2) - 1.0;
    float y = float((vertexID & 2) << 1) - 1.0;
    VertexOut out;
    out.position = float4(x, y, 0.0, 1.0);
    out.texCoords = float2((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// =============================================================================
// Fragment Shaders
// =============================================================================

// With background image texture (kept for compatibility, but applies grading too)
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

// Main cyberpunk relighting shader (no background replacement)
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

    // Edge detection from mask for glow effect
    float feather = uniforms.edgeFeather;
    float alpha = smoothstep(uniforms.maskThreshold - feather, uniforms.maskThreshold + feather, mask);

    // Sample neighboring mask values for edge detection
    float2 texelSize = float2(1.0 / videoTexture.get_width(), 1.0 / videoTexture.get_height());
    float maskL = maskTexture.sample(texSampler, uv + float2(-texelSize.x * 3.0, 0.0)).r;
    float maskR = maskTexture.sample(texSampler, uv + float2(texelSize.x * 3.0, 0.0)).r;
    float maskU = maskTexture.sample(texSampler, uv + float2(0.0, -texelSize.y * 3.0)).r;
    float maskD = maskTexture.sample(texSampler, uv + float2(0.0, texelSize.y * 3.0)).r;

    // Edge magnitude (Sobel-like)
    float edgeH = abs(maskL - maskR);
    float edgeV = abs(maskU - maskD);
    float edgeMask = saturate((edgeH + edgeV) * 2.0);

    // Get base color
    float3 color = videoColor.rgb;

    // === CINEMATIC SCI-FI COLOR GRADE ===
    // Clean, professional look inspired by sci-fi films (Blade Runner 2049, Ex Machina, etc.)

    // 1. Subtle sharpening first
    float3 blur = float3(0.0);
    blur += videoTexture.sample(texSampler, uv + float2(-texelSize.x, 0.0)).rgb;
    blur += videoTexture.sample(texSampler, uv + float2(texelSize.x, 0.0)).rgb;
    blur += videoTexture.sample(texSampler, uv + float2(0.0, -texelSize.y)).rgb;
    blur += videoTexture.sample(texSampler, uv + float2(0.0, texelSize.y)).rgb;
    blur *= 0.25;
    color = color + (color - blur) * 0.6; // Subtle sharpening
    color = clamp(color, 0.0, 1.0);

    // 2. Get luminance for tonal adjustments
    float luma = dot(color, float3(0.2126, 0.7152, 0.0722));

    // 3. Contrast enhancement (S-curve)
    // Crush blacks slightly, boost highlights
    float3 contrasted = color;
    contrasted = (contrasted - 0.5) * 1.15 + 0.5; // Boost contrast
    contrasted = clamp(contrasted, 0.0, 1.0);

    // 4. Lift shadows slightly (cinematic black level)
    contrasted = max(contrasted, 0.03); // Lifted blacks - film look

    // 5. Cool shadows, neutral highlights (teal & orange inspired but subtle)
    float shadowMask = 1.0 - smoothstep(0.0, 0.35, luma);
    float highlightMask = smoothstep(0.65, 1.0, luma);

    // Subtle cool tint in shadows (slight blue/teal)
    contrasted.r -= shadowMask * 0.02;
    contrasted.b += shadowMask * 0.03;

    // Very subtle warm tint in highlights
    contrasted.r += highlightMask * 0.02;
    contrasted.b -= highlightMask * 0.01;

    // 6. Slight desaturation for cinematic feel
    float3 desat = mix(float3(luma), contrasted, 0.88); // 12% desaturation

    // 7. Subtle color temperature shift (slightly cool overall)
    desat.b += 0.015;
    desat.r -= 0.01;

    // 8. Highlight bloom/glow (subtle)
    float bloomMask = smoothstep(0.75, 1.0, luma);
    desat = mix(desat, min(desat * 1.1, 1.0), bloomMask * 0.3);

    float3 bgColor = clamp(desat, 0.0, 1.0);

    // Person gets same grade (unified look)
    float3 personColor = bgColor;

    // Blend based on mask
    float3 result = mix(bgColor, personColor, alpha);

    // Edge glow disabled - cleaner look
    // result = apply_edge_glow(result, mask, edgeMask, uniforms.time);

    // Very subtle vignette (almost invisible, just gentle falloff)
    float2 vignetteUV = uv - 0.5;
    float vignette = 1.0 - dot(vignetteUV, vignetteUV) * 0.3;
    vignette = smoothstep(0.3, 1.0, vignette);
    result *= mix(0.9, 1.0, vignette); // Much gentler: 90% at corners vs 70% before

    // Very subtle scanlines (barely visible, adds texture)
    float scanline = sin(uv.y * 600.0) * 0.5 + 0.5;
    scanline = smoothstep(0.4, 0.6, scanline);
    result *= mix(0.99, 1.0, scanline); // Almost invisible: 99% vs 97%

    return float4(result, 1.0);
}

// Passthrough only (no segmentation available)
fragment float4 colorgrade_only_fragment(
    VertexOut in [[stage_in]],
    texture2d<float> videoTexture [[texture(0)]],
    sampler texSampler [[sampler(0)]],
    constant CompositorUniforms &uniforms [[buffer(0)]]
) {
    float4 color = videoTexture.sample(texSampler, in.texCoords);
    // Apply color grading even without mask
    float3 graded = cyberpunk_color_grade(color.rgb, uniforms.time);
    return float4(graded, 1.0);
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

    outputMask.write(float4(blended, 0.0, 1.0, 1.0), gid);
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
