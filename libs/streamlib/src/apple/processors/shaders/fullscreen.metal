#include <metal_stdlib>
using namespace metal;

// Vertex output from vertex shader to fragment shader
struct VertexOut {
    float4 position [[position]];
    float2 texCoords;
};

// Vertex shader - generates fullscreen triangle without vertex buffers
vertex VertexOut vertex_main(uint vertexID [[vertex_id]]) {
    // Generate fullscreen triangle using vertex ID
    // Triangle covers entire screen: (-1,-1) to (3,3)
    float x = float((vertexID & 1) << 2) - 1.0;
    float y = float((vertexID & 2) << 1) - 1.0;

    VertexOut out;
    out.position = float4(x, y, 0.0, 1.0);
    out.texCoords = float2((x + 1.0) * 0.5, (1.0 - y) * 0.5);

    return out;
}

// Fragment shader - samples video texture and outputs color
// Handles both RGBA and BGRA texture formats by swizzling if needed
fragment float4 fragment_main(
    VertexOut in [[stage_in]],
    texture2d<float> videoTexture [[texture(0)]],
    sampler videoSampler [[sampler(0)]],
    constant int &isRGBA [[buffer(0)]]
) {
    float4 color = videoTexture.sample(videoSampler, in.texCoords);

    // Swizzle if texture is RGBA but display expects BGRA
    // If isRGBA == 1, swap R and B channels (RGBA -> BGRA)
    if (isRGBA == 1) {
        return float4(color.b, color.g, color.r, color.a);
    }

    return color;
}
