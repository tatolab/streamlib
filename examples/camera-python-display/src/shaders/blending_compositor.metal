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

// Alpha blend fragment shader
// Layer order: video (base) -> lower_third -> watermark (top)
fragment float4 blending_fragment(
    VertexOut in [[stage_in]],
    texture2d<float> videoTexture [[texture(0)]],
    texture2d<float> lowerThirdTexture [[texture(1)]],
    texture2d<float> watermarkTexture [[texture(2)]],
    sampler textureSampler [[sampler(0)]],
    constant uint &hasLowerThird [[buffer(0)]],
    constant uint &hasWatermark [[buffer(1)]]
) {
    float2 uv = in.texCoord;

    // Base layer: video
    float4 result = videoTexture.sample(textureSampler, uv);

    // Middle layer: lower third (alpha blend)
    if (hasLowerThird) {
        float4 lowerThird = lowerThirdTexture.sample(textureSampler, uv);
        // Standard alpha blending: out = src * src.a + dst * (1 - src.a)
        result.rgb = lowerThird.rgb * lowerThird.a + result.rgb * (1.0 - lowerThird.a);
        result.a = lowerThird.a + result.a * (1.0 - lowerThird.a);
    }

    // Top layer: watermark (alpha blend)
    if (hasWatermark) {
        float4 watermark = watermarkTexture.sample(textureSampler, uv);
        result.rgb = watermark.rgb * watermark.a + result.rgb * (1.0 - watermark.a);
        result.a = watermark.a + result.a * (1.0 - watermark.a);
    }

    return result;
}
