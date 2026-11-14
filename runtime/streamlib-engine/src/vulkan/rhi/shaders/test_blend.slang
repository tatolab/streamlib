// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// Test shader for the multi-input compute-kernel RHI: reads INPUT_COUNT
// storage buffers, writes their per-element sum into an output storage
// buffer. Compiled by build.rs into 4 SPIR-V variants (1, 2, 4, 8 inputs)
// with `INPUT_COUNT` defined to N, used by parameterized tests in
// `vulkan_compute_kernel.rs` to validate descriptor-set construction and
// dispatch correctness across binding counts.

#ifndef INPUT_COUNT
#error "Define INPUT_COUNT to 1, 2, 4, or 8"
#endif

#if INPUT_COUNT >= 1
[[vk::binding(0, 0)]] StructuredBuffer<uint> in0;
#endif
#if INPUT_COUNT >= 2
[[vk::binding(1, 0)]] StructuredBuffer<uint> in1;
#endif
#if INPUT_COUNT >= 4
[[vk::binding(2, 0)]] StructuredBuffer<uint> in2;
[[vk::binding(3, 0)]] StructuredBuffer<uint> in3;
#endif
#if INPUT_COUNT >= 8
[[vk::binding(4, 0)]] StructuredBuffer<uint> in4;
[[vk::binding(5, 0)]] StructuredBuffer<uint> in5;
[[vk::binding(6, 0)]] StructuredBuffer<uint> in6;
[[vk::binding(7, 0)]] StructuredBuffer<uint> in7;
#endif

// Output binding always sits one slot past the largest possible input
// (binding 8) so all four variants share an identical output-binding index.
[[vk::binding(8, 0)]] RWStructuredBuffer<uint> out_buf;

struct BlendPushConstants {
    uint element_count;
};
[[vk::push_constant]] ConstantBuffer<BlendPushConstants> pc;

[numthreads(64, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint i = invocation.x;
    if (i >= pc.element_count) return;

    uint sum = 0;
#if INPUT_COUNT >= 1
    sum += in0[i];
#endif
#if INPUT_COUNT >= 2
    sum += in1[i];
#endif
#if INPUT_COUNT >= 4
    sum += in2[i];
    sum += in3[i];
#endif
#if INPUT_COUNT >= 8
    sum += in4[i];
    sum += in5[i];
    sum += in6[i];
    sum += in7[i];
#endif
    out_buf[i] = sum;
}
