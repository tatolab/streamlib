// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const CLUSTERED_SUBGROUP_REDUCTION_SLANG: &str = r#"
[[vk::binding(0, 0)]] RWStructuredBuffer<uint> reduced_values;
uint clustered_add_over_four_lanes(uint value) {
return spirv_asm {
    OpCapability GroupNonUniformArithmetic;
    OpCapability GroupNonUniformClustered;
    OpGroupNonUniformIAdd $$uint result Subgroup ClusteredReduce $value $(4U)
};
}
[numthreads(64, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
uint lane = invocation.x;
reduced_values[lane] = clustered_add_over_four_lanes(reduced_values[lane]);
}
"#;
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_subgroup_operation_is_built_where_the_driver_serves_it_and_refused_by_name_where_not() {
    let device = vulkan_device_for_dispatch_tests();
    let spv = crate::core::rhi::SlangShaderSourceCompiler::new()
        .compile_or_reuse(
            CLUSTERED_SUBGROUP_REDUCTION_SLANG,
            crate::core::rhi::ShaderCompilationTargetStage::Compute,
            "main",
            "clustered-subgroup-reduction",
        )
        .expect("the clustered reduction compiles");
    let (bindings, push_constant_size) =
        crate::core::rhi::derive_bindings_from_spirv(&spv).expect("the blob reflects");
    let built = VulkanComputeKernel::new(
        &device,
        &ComputeKernelDescriptor {
            label: "clustered-subgroup-reduction",
            spv: &spv,
            entry_point: "main",
            bindings: &bindings,
            push_constant_size,
            specialization_constants: &[],
        },
    );
    let support = device.subgroup_operation_support();
    if support
        .supported_stages
        .contains(vk::ShaderStageFlags::COMPUTE)
        && support
            .supported_operations
            .contains(vk::SubgroupFeatureFlags::BASIC | vk::SubgroupFeatureFlags::CLUSTERED)
    {
        built.expect("a driver serving clustered operations builds the kernel");
    } else {
        let refusal = built
            .expect_err("a driver not serving clustered operations refuses at construction")
            .to_string();
        assert!(refusal.contains(&support.driver_name), "{refusal}");
        assert!(
            refusal.contains("clustered-subgroup-reduction"),
            "{refusal}"
        );
    }
}

#[test]
fn a_shader_whose_only_set_is_not_set_0_is_refused_at_validation() {
    let moved_to_set_1 = move_binding_to_another_descriptor_set_in_spirv_module(blend_spv(2), 0, 1);
    let bindings = blend_descriptor(2);
    let descriptor = ComputeKernelDescriptor {
        label: "blend-in-set-1",
        spv: &moved_to_set_1,
        entry_point: "main",
        bindings: &bindings,
        push_constant_size: 0,
        specialization_constants: &[],
    };
    let refusal = validate_against_spirv(
        &descriptor,
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("a binding outside set 0 cannot be bound, so it cannot be dropped in silence");
    let message = refusal.to_string();
    assert!(
        message.contains("only descriptor set 0 is supported") && message.contains('1'),
        "the refusal must name the unsupported set: {message}"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn kernel_bindings_reflect_descriptor() {
    let device = vulkan_device_for_dispatch_tests();
    for &input_count in &[1u32, 2, 4, 8] {
        let bindings = blend_descriptor(input_count);
        let kernel = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                entry_point: "main",
                label: "binding-shape",
                spv: blend_spv(input_count),
                bindings: &bindings,
                push_constant_size: 4,
                specialization_constants: &[],
            },
        )
        .expect("kernel creation");
        assert_eq!(
            kernel.bindings().len(),
            input_count as usize + 1,
            "expected {input_count}+1 bindings for {input_count}-input variant"
        );
        assert_eq!(kernel.push_constant_size(), 4);
        for spec in kernel.bindings() {
            assert_eq!(spec.kind, ComputeBindingKind::StorageBuffer);
        }
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn rejects_descriptor_with_mismatched_binding_kind() {
    let device = vulkan_device_for_dispatch_tests();
    // SPIR-V binding 0 is StorageBuffer — declaring it as UniformBuffer must fail.
    let bindings = vec![
        ComputeBindingSpec::uniform_buffer(0),
        ComputeBindingSpec::storage_buffer(8),
    ];
    let result = VulkanComputeKernel::new(
        &device,
        &ComputeKernelDescriptor {
            entry_point: "main",
            label: "kind-mismatch",
            spv: blend_spv(1),
            bindings: &bindings,
            push_constant_size: 4,
            specialization_constants: &[],
        },
    );
    let err = result.expect_err("expected validation failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("binding 0") && msg.contains("UniformBuffer"),
        "expected mismatch error mentioning binding 0 and UniformBuffer, got: {msg}"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn rejects_descriptor_missing_a_binding_the_shader_declares() {
    let device = vulkan_device_for_dispatch_tests();
    // 4-input shader declares bindings 0..3 + 8; omit binding 2.
    let bindings = vec![
        ComputeBindingSpec::storage_buffer(0),
        ComputeBindingSpec::storage_buffer(1),
        ComputeBindingSpec::storage_buffer(3),
        ComputeBindingSpec::storage_buffer(8),
    ];
    let result = VulkanComputeKernel::new(
        &device,
        &ComputeKernelDescriptor {
            entry_point: "main",
            label: "missing-binding",
            spv: blend_spv(4),
            bindings: &bindings,
            push_constant_size: 4,
            specialization_constants: &[],
        },
    );
    let err = result.expect_err("expected validation failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("binding 2"),
        "expected error about missing binding 2, got: {msg}"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn rejects_descriptor_with_extra_binding_not_in_shader() {
    let device = vulkan_device_for_dispatch_tests();
    // 1-input shader declares only bindings 0 and 8 — extra binding 1 is invalid.
    let bindings = vec![
        ComputeBindingSpec::storage_buffer(0),
        ComputeBindingSpec::storage_buffer(1),
        ComputeBindingSpec::storage_buffer(8),
    ];
    let result = VulkanComputeKernel::new(
        &device,
        &ComputeKernelDescriptor {
            entry_point: "main",
            label: "extra-binding",
            spv: blend_spv(1),
            bindings: &bindings,
            push_constant_size: 4,
            specialization_constants: &[],
        },
    );
    let err = result.expect_err("expected validation failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("binding 1"),
        "expected error about extra binding 1, got: {msg}"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn rejects_push_constant_size_mismatch() {
    let device = vulkan_device_for_dispatch_tests();
    let bindings = blend_descriptor(1);
    let result = VulkanComputeKernel::new(
        &device,
        &ComputeKernelDescriptor {
            entry_point: "main",
            label: "push-size-mismatch",
            spv: blend_spv(1),
            bindings: &bindings,
            push_constant_size: 16, // SPIR-V declares 4 bytes
            specialization_constants: &[],
        },
    );
    let err = result.expect_err("expected validation failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("push-constant size mismatch"),
        "expected push-constant size error, got: {msg}"
    );
}
