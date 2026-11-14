// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only
//
// NV12 to RGB(A) compute shader.
//
// Input:  Combined image sampler with VkSamplerYcbcrConversion — the hardware
//         handles plane separation, chroma upsampling (4:2:0 → 4:4:4), and
//         YCbCr → RGB conversion automatically.
// Output: RGBA storage image (R8G8B8A8_UNORM).
//
// Each thread converts one pixel. The YCbCr sampler does all the heavy
// lifting — the shader just reads and writes.

// Combined image sampler with YCbCr conversion (immutable sampler in
// descriptor set layout). Sampling through it returns linear RGB.
[[vk::binding(0, 0)]] Sampler2D nv12Input;

// RGBA output — regular storage image write.
[[vk::binding(1, 0)]] [format("rgba8")] WTexture2D<float4> rgbOutput;

struct Nv12ToRgbRegisters {
    int2 resolution;
};
[[vk::push_constant]] ConstantBuffer<Nv12ToRgbRegisters> registers;

[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID)
{
    int2 coord = int2(invocation.xy);

    if (coord.x >= registers.resolution.x || coord.y >= registers.resolution.y)
        return;

    // Normalized coordinates with half-pixel offset for texel centers.
    float2 uv = (float2(coord) + 0.5) / float2(registers.resolution);

    // The YCbCr sampler converts NV12 → RGB automatically.
    float4 color = nv12Input.SampleLevel(uv, 0.0);

    rgbOutput.Store(uint2(coord), float4(color.rgb, 1.0));
}
