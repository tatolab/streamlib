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

// Uniform buffer for layer availability flags
struct BlendingUniforms {
    uint hasVideo;
    uint hasLowerThird;
    uint hasWatermark;
    uint _padding;
};

// Porter-Duff "over" alpha compositing for premultiplied alpha textures
// Layer order: video (base) -> lower_third -> watermark (top)
//
// Photoshop-style layer stacking: each layer composites on top of the result below.
// Formula for premultiplied alpha (Skia GPU output):
//   out.rgb = src.rgb + dst.rgb * (1 - src.a)
//   out.a   = src.a   + dst.a   * (1 - src.a)
fragment float4 blending_fragment(
    VertexOut in [[stage_in]],
    texture2d<float> videoTexture [[texture(0)]],
    texture2d<float> lowerThirdTexture [[texture(1)]],
    texture2d<float> watermarkTexture [[texture(2)]],
    sampler textureSampler [[sampler(0)]],
    constant BlendingUniforms &uniforms [[buffer(0)]]
) {
    float2 uv = in.texCoord;

    // Base layer: video (or black if not yet available)
    float4 result = uniforms.hasVideo ? videoTexture.sample(textureSampler, uv) : float4(0.0, 0.0, 0.0, 1.0);

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

    return result;
}
