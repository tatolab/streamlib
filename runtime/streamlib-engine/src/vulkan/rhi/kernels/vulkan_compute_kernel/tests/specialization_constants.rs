// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::rhi::{ShaderSpecializationConstant, ShaderSpecializationValue};

/// Each invocation of one workgroup writes the workgroup's size at its own
/// index, so the count of written entries is the size the pipeline was built
/// with.
const WORKGROUP_SIZE_SLANG: &str = r#"
[vk::constant_id(0)] const uint WORKGROUP_SIZE = 64;
[vk::constant_id(1)] const float UNUSED_SCALE = 1.0;
[[vk::binding(0, 0)]] RWStructuredBuffer<uint> written;
[numthreads(WORKGROUP_SIZE, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    written[invocation.x] = WORKGROUP_SIZE + uint(UNUSED_SCALE) - 1;
}
"#;

const BUFFER_ELEMENT_COUNT: u32 = 128;

fn workgroup_size_spv() -> std::sync::Arc<[u8]> {
    crate::core::rhi::SlangShaderSourceCompiler::new()
        .compile_or_reuse(
            WORKGROUP_SIZE_SLANG,
            crate::core::rhi::ShaderCompilationTargetStage::Compute,
            "main",
            "specialized-workgroup-size",
        )
        .expect("the specialized shader compiles")
}

fn descriptor_specialized<'a>(
    spv: &'a [u8],
    bindings: &'a [ComputeBindingSpec],
    specialization_constants: &'a [ShaderSpecializationConstant],
) -> ComputeKernelDescriptor<'a> {
    ComputeKernelDescriptor {
        label: "specialized-workgroup-size",
        spv,
        entry_point: "main",
        bindings,
        push_constant_size: 0,
        specialization_constants,
    }
}

fn workgroup_size(value: ShaderSpecializationValue) -> [ShaderSpecializationConstant; 1] {
    [ShaderSpecializationConstant {
        constant_id: 0,
        value,
    }]
}

fn validation_refusal(specialization_constants: &[ShaderSpecializationConstant]) -> String {
    let spv = workgroup_size_spv();
    let bindings = [ComputeBindingSpec::storage_buffer(0)];
    validate_against_spirv(
        &descriptor_specialized(&spv, &bindings, specialization_constants),
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("the specialization must be refused")
    .to_string()
}

#[test]
fn a_constant_the_shader_does_not_declare_is_refused_naming_the_ones_it_does() {
    let refusal = validation_refusal(&[ShaderSpecializationConstant {
        constant_id: 7,
        value: ShaderSpecializationValue::U32(32),
    }]);
    assert!(
        refusal.contains("specialization constant 7")
            && refusal.contains("`WORKGROUP_SIZE`")
            && refusal.contains("`UNUSED_SCALE`"),
        "{refusal}"
    );
}

#[test]
fn a_constant_of_another_type_than_the_shaders_is_refused() {
    let refusal = validation_refusal(&workgroup_size(ShaderSpecializationValue::F32(32.0)));
    assert!(
        refusal.contains("`WORKGROUP_SIZE`")
            && refusal.contains("as float32")
            && refusal.contains("declares it uint32"),
        "{refusal}"
    );
}

#[test]
fn a_constant_set_twice_is_refused() {
    let refusal = validation_refusal(&[
        ShaderSpecializationConstant {
            constant_id: 0,
            value: ShaderSpecializationValue::U32(32),
        },
        ShaderSpecializationConstant {
            constant_id: 0,
            value: ShaderSpecializationValue::U32(16),
        },
    ]);
    assert!(refusal.contains("set twice"), "{refusal}");
}

/// The entries one workgroup of a kernel specialized to `specialization`
/// writes.
fn entries_one_workgroup_writes(
    device: &Arc<HostVulkanDevice>,
    spv: &[u8],
    specialization: &[ShaderSpecializationConstant],
) -> Vec<u32> {
    let bindings = [ComputeBindingSpec::storage_buffer(0)];
    let kernel = VulkanComputeKernel::new(
        device,
        &descriptor_specialized(spv, &bindings, specialization),
    )
    .expect("the specialized kernel builds");
    let written = make_storage_buffer(device, BUFFER_ELEMENT_COUNT);
    write_buffer_u32(&written, &[0; BUFFER_ELEMENT_COUNT as usize]);
    kernel
        .set_storage_buffer_pixel(0, &written)
        .expect("the buffer binds");
    kernel.dispatch(1, 1, 1).expect("one workgroup dispatches");
    read_buffer_u32(&written, BUFFER_ELEMENT_COUNT as usize)
}

fn the_first_entries_set_to(set_count: u32) -> Vec<u32> {
    (0..BUFFER_ELEMENT_COUNT)
        .map(|index| if index < set_count { set_count } else { 0 })
        .collect()
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn one_shader_specialized_to_two_workgroup_sizes_dispatches_each_size() {
    let device = vulkan_device_for_dispatch_tests();
    let spv = workgroup_size_spv();
    assert_eq!(
        entries_one_workgroup_writes(
            &device,
            &spv,
            &workgroup_size(ShaderSpecializationValue::U32(32))
        ),
        the_first_entries_set_to(32)
    );
    assert_eq!(
        entries_one_workgroup_writes(
            &device,
            &spv,
            &workgroup_size(ShaderSpecializationValue::U32(96))
        ),
        the_first_entries_set_to(96)
    );
    assert_eq!(
        entries_one_workgroup_writes(&device, &spv, &[]),
        the_first_entries_set_to(64),
        "a constant left unset keeps the shader's default"
    );
}

/// Push constants whose array is sized by a specialization constant: the
/// reflected size is the default's, not the size the pipeline is built with.
const PUSH_CONSTANTS_SIZED_BY_A_CONSTANT_SLANG: &str = r#"
[vk::constant_id(0)] const uint TAP_COUNT = 4;
struct Taps { uint weights[TAP_COUNT]; };
[[vk::push_constant]] ConstantBuffer<Taps> taps;
[[vk::binding(0, 0)]] RWStructuredBuffer<uint> written;
[numthreads(1, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    written[0] = taps.weights[TAP_COUNT - 1];
}
"#;

/// A local array sized by a specialization constant, outside every binding.
const LOCAL_ARRAY_SIZED_BY_A_CONSTANT_SLANG: &str = r#"
[vk::constant_id(0)] const uint TAP_COUNT = 4;
[[vk::binding(0, 0)]] RWStructuredBuffer<uint> written;
[numthreads(1, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint taps[TAP_COUNT];
    for (uint tap = 0; tap < TAP_COUNT; tap++) { taps[tap] = tap; }
    written[0] = taps[TAP_COUNT - 1];
}
"#;

fn compiled(source: &str, label: &str) -> std::sync::Arc<[u8]> {
    crate::core::rhi::SlangShaderSourceCompiler::new()
        .compile_or_reuse(
            source,
            crate::core::rhi::ShaderCompilationTargetStage::Compute,
            "main",
            label,
        )
        .expect("the shader compiles")
}

fn validated(spv: &[u8], push_constant_size: u32) -> crate::core::Result<Vec<ComputeBindingSpec>> {
    let bindings = [ComputeBindingSpec::storage_buffer(0)];
    validate_against_spirv(
        &ComputeKernelDescriptor {
            label: "array-sized-by-a-constant",
            spv,
            entry_point: "main",
            bindings: &bindings,
            push_constant_size,
            specialization_constants: &workgroup_size(ShaderSpecializationValue::U32(8)),
        },
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
}

#[test]
fn push_constants_sized_by_a_specialization_constant_are_refused() {
    let spv = compiled(
        PUSH_CONSTANTS_SIZED_BY_A_CONSTANT_SLANG,
        "push-constants-sized-by-a-constant",
    );
    let refusal = validated(&spv, 32)
        .expect_err("the layout cannot be checked against the specialized size")
        .to_string();
    assert!(
        refusal.contains("sizes an array in its bindings or push constants"),
        "{refusal}"
    );
    assert!(
        crate::core::rhi::derive_bindings_from_spirv(&spv).is_err(),
        "the escalate path, which reflects the shape alone, refuses it too"
    );
}

#[test]
fn a_local_array_sized_by_a_specialization_constant_is_taken() {
    let spv = compiled(
        LOCAL_ARRAY_SIZED_BY_A_CONSTANT_SLANG,
        "local-array-sized-by-a-constant",
    );
    validated(&spv, 0).expect("an array outside the bindings and push constants is taken");
}

/// Writes the low word of a 64-bit constant and a 64-bit float constant's
/// integer part, so both reach the shader as the values the pipeline took.
const SIXTY_FOUR_BIT_CONSTANTS_SLANG: &str = r#"
[vk::constant_id(0)] const uint64_t OFFSET = 1;
[vk::constant_id(1)] const double SCALE = 1.0;
[[vk::binding(0, 0)]] RWStructuredBuffer<uint> written;
[numthreads(1, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    written[0] = uint(OFFSET & 0xffffffff);
    written[1] = uint(SCALE);
}
"#;

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn sixty_four_bit_constants_reach_the_shader_on_a_device_serving_them() {
    let device = vulkan_device_for_dispatch_tests();
    let spv = compiled(SIXTY_FOUR_BIT_CONSTANTS_SLANG, "sixty-four-bit-constants");
    let written = entries_one_workgroup_writes(
        &device,
        &spv,
        &[
            ShaderSpecializationConstant {
                constant_id: 0,
                value: ShaderSpecializationValue::U64((7 << 32) | 40),
            },
            ShaderSpecializationConstant {
                constant_id: 1,
                value: ShaderSpecializationValue::F64(12.0),
            },
        ],
    );
    assert_eq!(&written[..2], &[40, 12]);
}
