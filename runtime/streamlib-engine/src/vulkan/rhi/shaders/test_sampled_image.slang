// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Test shader for the SampledImage binding kind (Vulkan
// `VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE` — a `Texture2D`, no combined
// sampler). Exercises `VulkanComputeKernel::set_sampled_image_view` end-
// to-end via descriptor-layout creation, descriptor write, bind, and
// dispatch. The shader body is a no-op texel load; we only need the
// declared binding to exist so the kernel sees a real SAMPLED_IMAGE
// slot through SPIR-V reflection.

[[vk::binding(0, 0)]] Texture2D<float4> uImage;
[[vk::binding(1, 0)]] [format("rgba8")] WTexture2D<float4> uOut;

[numthreads(1, 1, 1)]
void main() {
    // A texel load at an explicit level — no sampler required. The
    // dispatch is single-thread.
    uOut.Store(uint2(0, 0), uImage.Load(int3(0, 0, 0)));
}
