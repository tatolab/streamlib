// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Pipeline-compiler pre-warm probe. NOT modeled on any specific kernel.
//
// On NVIDIA Linux the shader backend compiler (`libnvidia-gpucomp`) does a
// lazy `pthread_once` GLOBAL init the first time a pipeline that requires
// real codegen is created in a process. That init is `pthread_once`, so it
// runs exactly once per process — but when it first fires during concurrent
// multi-plugin processor setup it double-frees its own heap and aborts.
// `HostVulkanDevice::prewarm_pipeline_compiler` builds (and drops) this
// pipeline single-threaded at device construction so the compiler comes
// online in a controlled state; every later pipeline of any shape then
// finds it already initialized.
//
// The trigger is "a pipeline whose shader requires real codegen", NOT a
// particular descriptor layout — a no-op shader can be fast-pathed without
// engaging the compiler. So this probe is deliberately a kitchen sink: it
// touches every compute descriptor kind (two storage buffers, a uniform
// buffer, a sampled texture, a storage image) plus the spec-minimum-maximum
// 128-byte push-constant block, and does enough arithmetic + a texture
// fetch + an image store that the driver cannot fast-path it. The point is
// to force the compiler init regardless of which feature happens to engage
// codegen — do not trim it down to match whatever kernel prompted it.

[[vk::binding(0, 0)]] StructuredBuffer<uint> in0;
[[vk::binding(1, 0)]] StructuredBuffer<uint> in1;

struct PrewarmParameters {
    uint scale;
};
[[vk::binding(2, 0)]] ConstantBuffer<PrewarmParameters> params;

[[vk::binding(3, 0)]] Sampler2D src_tex;
[[vk::binding(4, 0)]] [format("rgba8")] WTexture2D<float4> dst_image;

struct PrewarmPushConstants {
    uint values[32]; // 128 bytes
};
[[vk::push_constant]] ConstantBuffer<PrewarmPushConstants> pc;

[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    // An explicit level — compute shaders have no implicit derivatives.
    float t = src_tex.SampleLevel(float2(0.5), 0.0).r;
    uint acc = in0[0] + in1[0] + params.scale + pc.values[0]
             + uint(t * 255.0);
    dst_image.Store(invocation.xy, float4(float(acc & 0xFFu) / 255.0));
}
