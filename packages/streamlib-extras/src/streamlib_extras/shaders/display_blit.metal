/*
 * Metal shader for fullscreen texture blit to CAMetalLayer drawable with text overlay support.
 *
 * Uses a fullscreen triangle optimization (3 vertices instead of 4-vertex quad).
 * This is slightly more efficient as it avoids the diagonal split.
 */

#include <metal_stdlib>
using namespace metal;

struct VertexOut {
    float4 position [[position]];
    float2 texCoord;
};

/*
 * Vertex shader - generates fullscreen triangle without vertex buffer.
 *
 * The triangle covers the entire screen using just 3 vertices:
 * - Bottom-left: (-1, -1)
 * - Bottom-right: (3, -1) - extends offscreen
 * - Top-left: (-1, 3) - extends offscreen
 *
 * The rasterizer clips the offscreen parts, leaving a perfect fullscreen quad.
 */
vertex VertexOut vertex_main(uint vertexID [[vertex_id]]) {
    float2 positions[3] = {
        float2(-1.0, -1.0),  // bottom-left
        float2( 3.0, -1.0),  // bottom-right (offscreen)
        float2(-1.0,  3.0),  // top-left (offscreen)
    };

    float2 texCoords[3] = {
        float2(0.0, 1.0),  // bottom-left (flipped Y for Metal)
        float2(2.0, 1.0),  // bottom-right (offscreen)
        float2(0.0, -1.0), // top-left (offscreen)
    };

    VertexOut out;
    out.position = float4(positions[vertexID], 0.0, 1.0);
    out.texCoord = texCoords[vertexID];
    return out;
}

/*
 * Fragment shader - samples input texture and outputs to drawable.
 *
 * Uses linear filtering for smooth scaling if window size differs from texture size.
 */
fragment float4 fragment_main(
    VertexOut in [[stage_in]],
    texture2d<float> inputTexture [[texture(0)]]
) {
    constexpr sampler textureSampler(
        mag_filter::linear,
        min_filter::linear,
        address::clamp_to_edge
    );

    return inputTexture.sample(textureSampler, in.texCoord);
}

/*
 * Text overlay shaders - renders a positioned quad with alpha blending.
 */

vertex VertexOut vertex_text_overlay(
    uint vertexID [[vertex_id]],
    constant float2& screenSize [[buffer(0)]],
    constant float2& textPosition [[buffer(1)]],
    constant float2& textSize [[buffer(2)]]
) {
    // Quad vertices (two triangles)
    float2 quadPositions[6] = {
        float2(0.0, 0.0),  // bottom-left
        float2(1.0, 0.0),  // bottom-right
        float2(0.0, 1.0),  // top-left
        float2(0.0, 1.0),  // top-left
        float2(1.0, 0.0),  // bottom-right
        float2(1.0, 1.0),  // top-right
    };

    float2 texCoords[6] = {
        float2(0.0, 0.0),  // bottom-left (flipped for PIL image)
        float2(1.0, 0.0),  // bottom-right
        float2(0.0, 1.0),  // top-left
        float2(0.0, 1.0),  // top-left
        float2(1.0, 0.0),  // bottom-right
        float2(1.0, 1.0),  // top-right
    };

    // Calculate position in pixel coordinates
    float2 pixelPos = textPosition + quadPositions[vertexID] * textSize;

    // Convert to NDC
    float2 ndc = (pixelPos / screenSize) * 2.0 - 1.0;
    ndc.y = -ndc.y;  // Flip Y

    VertexOut out;
    out.position = float4(ndc, 0.0, 1.0);
    out.texCoord = texCoords[vertexID];
    return out;
}

fragment float4 fragment_text_overlay(
    VertexOut in [[stage_in]],
    texture2d<float> textTexture [[texture(0)]]
) {
    constexpr sampler textureSampler(mag_filter::linear, min_filter::linear);
    return textTexture.sample(textureSampler, in.texCoord);
}
