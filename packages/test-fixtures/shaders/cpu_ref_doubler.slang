// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

// Minimal "output[i] = input[i] * 2" compute shader used as the
// CPU-reference dispatch target for an integration test that dispatches
// the kernel against synthetic input and asserts every output value
// matches the trivial CPU-computed reference (`input[i] * 2`).
//
// Single-input + single-output SSBO + a u32 element-count push
// constant — the smallest shape that exercises the binding methods
// (`set_storage_buffer_storage` + `set_push_constants` + `dispatch`)
// end-to-end.

[[vk::binding(0, 0)]] StructuredBuffer<uint> in_buf;
[[vk::binding(1, 0)]] RWStructuredBuffer<uint> out_buf;

struct CpuRefDoublerPushConstants {
    uint element_count;
};
[[vk::push_constant]] ConstantBuffer<CpuRefDoublerPushConstants> pc;

[numthreads(64, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint i = invocation.x;
    if (i >= pc.element_count) return;
    out_buf[i] = in_buf[i] * 2u;
}
