// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// The pass the v1 compute wire could not express: read one surface, write a
// different one. Its two bindings are deliberately differently-named and
// differently-kinded, so a dispatch that resolved them by slot order rather
// than by name would bind them backwards and fail visibly.
//
// Output is the source with its RGB inverted, plus a push-constant bias, so a
// reader can tell "the kernel ran and read the right input" apart from both
// "the kernel never ran" and "the kernel read its own output".

[[vk::binding(0, 0)]] Sampler2D source_image;
[[vk::binding(1, 0)]] [format("rgba8")] WTexture2D<float4> output_image;

struct ReadOneWriteAnotherPushConstants {
    float bias;
};
[[vk::push_constant]] ConstantBuffer<ReadOneWriteAnotherPushConstants> pc;

[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint2 at = invocation.xy;
    uint2 extent;
    output_image.GetDimensions(extent.x, extent.y);
    if (at.x >= extent.x || at.y >= extent.y) {
        return;
    }
    float2 uv = (float2(at) + 0.5) / float2(extent);
    float4 source = source_image.SampleLevel(uv, 0.0);
    output_image.Store(at, float4(1.0 - source.rgb + pc.bias, source.a));
}
