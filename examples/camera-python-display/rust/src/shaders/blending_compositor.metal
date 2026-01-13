// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#include <metal_stdlib>
using namespace metal;

// Vertex output for fullscreen quad
struct VertexOut {
    float4 position [[position]];
    float2 texCoord;
};

// Fullscreen triangle (covers screen with single triangle)
vertex VertexOut blending_vertex(uint vertexID [[vertex_id]]) {
    VertexOut out;

    // Generate fullscreen triangle
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

// Uniform buffer for layer availability and PiP animation
struct BlendingUniforms {
    uint hasVideo;
    uint hasLowerThird;
    uint hasWatermark;
    uint hasPip;
    float pipSlideProgress;  // 0.0 = off-screen right, 1.0 = fully visible
    float _padding1;
    float _padding2;
    float _padding3;
};

// Cyberpunk color palette
constant float4 CYBER_CYAN = float4(0.0, 0.94, 1.0, 1.0);      // #00f0ff
constant float4 CYBER_YELLOW = float4(0.988, 0.933, 0.039, 1.0); // #fcee0a
constant float4 CYBER_WHITE = float4(1.0, 1.0, 1.0, 1.0);
constant float4 CYBER_DARK = float4(0.06, 0.06, 0.08, 0.95);     // Semi-transparent dark

// PiP configuration (in UV space, relative to screen)
constant float PIP_WIDTH = 0.28;           // 28% of screen width
constant float PIP_HEIGHT = 0.35;          // 35% of screen height
constant float PIP_MARGIN = 0.02;          // Margin from edge
constant float PIP_BORDER = 0.004;         // Border thickness
constant float TITLE_BAR_HEIGHT = 0.045;   // Title bar height

// Check if point is within a rectangle
bool inRect(float2 uv, float2 minCorner, float2 maxCorner) {
    return uv.x >= minCorner.x && uv.x <= maxCorner.x &&
           uv.y >= minCorner.y && uv.y <= maxCorner.y;
}

// Draw the PiP frame with Cyberpunk N54 News style
float4 drawPipFrame(
    float2 uv,
    float slideProgress,
    texture2d<float> pipTexture,
    sampler textureSampler,
    float4 baseColor
) {
    // Calculate animated PiP position (slides in from right)
    // slideOffset is positive when off-screen, 0 when fully visible
    float slideOffset = (1.0 - slideProgress) * (PIP_WIDTH + PIP_MARGIN + 0.1);

    // PiP content area (upper right) - ADD offset to push right (off-screen)
    float pipLeft = 1.0 - PIP_MARGIN - PIP_WIDTH + slideOffset;
    float pipRight = 1.0 - PIP_MARGIN + slideOffset;
    float pipTop = PIP_MARGIN;
    float pipBottom = PIP_MARGIN + PIP_HEIGHT;

    // Title bar below PiP content
    float titleTop = pipBottom;
    float titleBottom = pipBottom + TITLE_BAR_HEIGHT;

    // Outer frame (border around everything)
    float frameLeft = pipLeft - PIP_BORDER;
    float frameRight = pipRight + PIP_BORDER;
    float frameTop = pipTop - PIP_BORDER;
    float frameBottom = titleBottom + PIP_BORDER;

    float4 result = baseColor;

    // Check if we're in the frame area at all
    if (!inRect(uv, float2(frameLeft, frameTop), float2(frameRight, frameBottom))) {
        return result;
    }

    // Outer border (cyan glow)
    bool inOuterBorder = inRect(uv, float2(frameLeft, frameTop), float2(frameRight, frameBottom)) &&
                         !inRect(uv, float2(frameLeft + PIP_BORDER, frameTop + PIP_BORDER),
                                float2(frameRight - PIP_BORDER, frameBottom - PIP_BORDER));
    if (inOuterBorder) {
        return CYBER_CYAN;
    }

    // Inner white border
    float innerBorder = PIP_BORDER * 0.5;
    bool inInnerBorder = inRect(uv, float2(frameLeft + PIP_BORDER, frameTop + PIP_BORDER),
                                float2(frameRight - PIP_BORDER, frameBottom - PIP_BORDER)) &&
                         !inRect(uv, float2(frameLeft + PIP_BORDER + innerBorder, frameTop + PIP_BORDER + innerBorder),
                                float2(frameRight - PIP_BORDER - innerBorder, frameBottom - PIP_BORDER - innerBorder));
    if (inInnerBorder) {
        return CYBER_WHITE;
    }

    // Title bar background (yellow with slight transparency)
    if (inRect(uv, float2(pipLeft, titleTop), float2(pipRight, titleBottom))) {
        // Yellow title bar like in reference
        float4 titleColor = CYBER_YELLOW;
        titleColor.a = 0.95;

        // Add subtle scan line effect
        float scanLine = fract(uv.y * 200.0);
        if (scanLine < 0.1) {
            titleColor.rgb *= 0.9;
        }

        return titleColor;
    }

    // PiP content area - sample from pip texture
    if (inRect(uv, float2(pipLeft, pipTop), float2(pipRight, pipBottom))) {
        // Dark background first
        result = CYBER_DARK;

        // Map UV to PiP texture space
        float2 pipUV;
        pipUV.x = (uv.x - pipLeft) / (pipRight - pipLeft);
        pipUV.y = (uv.y - pipTop) / (pipBottom - pipTop);

        // Sample PiP texture
        float4 pipColor = pipTexture.sample(textureSampler, pipUV);

        // Blend with premultiplied alpha
        result.rgb = pipColor.rgb + result.rgb * (1.0 - pipColor.a);
        result.a = pipColor.a + result.a * (1.0 - pipColor.a);

        // Add corner tech decorations
        float cornerSize = 0.015;
        float techLineThickness = 0.002;
        float2 localUV = float2(pipUV.x * PIP_WIDTH, pipUV.y * PIP_HEIGHT);

        // Top-left corner lines
        if ((pipUV.x < cornerSize / PIP_WIDTH && pipUV.y < techLineThickness / PIP_HEIGHT) ||
            (pipUV.x < techLineThickness / PIP_WIDTH && pipUV.y < cornerSize / PIP_HEIGHT)) {
            result = CYBER_CYAN;
        }

        // Top-right corner
        if ((pipUV.x > 1.0 - cornerSize / PIP_WIDTH && pipUV.y < techLineThickness / PIP_HEIGHT) ||
            (pipUV.x > 1.0 - techLineThickness / PIP_WIDTH && pipUV.y < cornerSize / PIP_HEIGHT)) {
            result = CYBER_CYAN;
        }

        // Bottom-left corner
        if ((pipUV.x < cornerSize / PIP_WIDTH && pipUV.y > 1.0 - techLineThickness / PIP_HEIGHT) ||
            (pipUV.x < techLineThickness / PIP_WIDTH && pipUV.y > 1.0 - cornerSize / PIP_HEIGHT)) {
            result = CYBER_CYAN;
        }

        // Bottom-right corner
        if ((pipUV.x > 1.0 - cornerSize / PIP_WIDTH && pipUV.y > 1.0 - techLineThickness / PIP_HEIGHT) ||
            (pipUV.x > 1.0 - techLineThickness / PIP_WIDTH && pipUV.y > 1.0 - cornerSize / PIP_HEIGHT)) {
            result = CYBER_CYAN;
        }
    }

    return result;
}

// Porter-Duff "over" alpha compositing for premultiplied alpha textures
// Layer order: video (base) -> lower_third -> watermark -> pip (top)
fragment float4 blending_fragment(
    VertexOut in [[stage_in]],
    texture2d<float> videoTexture [[texture(0)]],
    texture2d<float> lowerThirdTexture [[texture(1)]],
    texture2d<float> watermarkTexture [[texture(2)]],
    texture2d<float> pipTexture [[texture(3)]],
    sampler textureSampler [[sampler(0)]],
    constant BlendingUniforms &uniforms [[buffer(0)]]
) {
    float2 uv = in.texCoord;

    // Base layer: video (or dark blue if not yet available)
    float4 result = uniforms.hasVideo ? videoTexture.sample(textureSampler, uv) : float4(0.05, 0.05, 0.12, 1.0);

    // Middle layer: lower third (premultiplied alpha blend)
    if (uniforms.hasLowerThird) {
        float4 src = lowerThirdTexture.sample(textureSampler, uv);
        result.rgb = src.rgb + result.rgb * (1.0 - src.a);
        result.a = src.a + result.a * (1.0 - src.a);
    }

    // Top layer: watermark (premultiplied alpha blend)
    if (uniforms.hasWatermark) {
        float4 src = watermarkTexture.sample(textureSampler, uv);
        result.rgb = src.rgb + result.rgb * (1.0 - src.a);
        result.a = src.a + result.a * (1.0 - src.a);
    }

    // PiP overlay (slides in from right when ready)
    if (uniforms.hasPip && uniforms.pipSlideProgress > 0.0) {
        result = drawPipFrame(uv, uniforms.pipSlideProgress, pipTexture, textureSampler, result);
    }

    return result;
}
