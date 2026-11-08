// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

[[vk::binding(0, 0)]] Sampler2D cameraTexture;

struct OffscreenBlitPushConstants {
    float2 scale;
    float2 offset;
};
[[vk::push_constant]] ConstantBuffer<OffscreenBlitPushConstants> pc;

float4 main([[vk::location(0)]] float2 inUV : TEXCOORD0) : SV_Target0 {
    // Aspect-ratio-aware sampling with letterbox/pillarbox black bars
    float2 texCoord = (inUV - 0.5) / pc.scale + 0.5 + pc.offset;

    if (texCoord.x < 0.0 || texCoord.x > 1.0 || texCoord.y < 0.0 || texCoord.y > 1.0) {
        return float4(0.0, 0.0, 0.0, 1.0);
    }

    return cameraTexture.Sample(texCoord);
}
