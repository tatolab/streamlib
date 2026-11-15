// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::rhi::spirv_module_rewriting_for_tests::{
    move_binding_to_another_descriptor_set_in_spirv_module, rename_binding_in_spirv_module,
    strip_every_debug_name_from_spirv_module,
};
use crate::core::rhi::{
    RayTracingBindingSpec, RayTracingKernelDescriptor, RayTracingPushConstants,
    RayTracingShaderGroup, RayTracingShaderStageFlags, RayTracingStage,
};
#[cfg(target_os = "linux")]
use crate::core::rhi::{
    Texture, TextureDescriptor, TextureFormat, TextureReadbackDescriptor, TextureSourceLayout,
    TextureUsages,
};
#[cfg(target_os = "linux")]
use crate::host_rhi::HostTextureExt;
use crate::vulkan::rhi::kernels::vulkan_kernel_capability_refusal::VulkanSubgroupOperationSupport;
use crate::vulkan::rhi::{HostVulkanDevice, VulkanAccelerationStructure};
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::{HostVulkanTexture, TlasInstanceDesc, VulkanTextureReadback};

/// Smoke check both that a Vulkan device is available AND that it
/// exposes the `VK_KHR_ray_tracing_pipeline` extension chain. Tests
/// that need RT skip cleanly when either is missing — no devices are
/// faked.
fn try_ray_tracing_device() -> Option<Arc<HostVulkanDevice>> {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            return None;
        }
    };
    if !device.supports_ray_tracing_pipeline() {
        println!("Skipping - device does not expose VK_KHR_ray_tracing_pipeline");
        return None;
    }
    Some(device)
}

fn rt_test_rgen_spv() -> &'static [u8] {
    include_bytes!(concat!(env!("OUT_DIR"), "/raytracing_test.rgen.spv"))
}
fn rt_test_rmiss_spv() -> &'static [u8] {
    include_bytes!(concat!(env!("OUT_DIR"), "/raytracing_test.rmiss.spv"))
}
fn rt_test_rchit_spv() -> &'static [u8] {
    include_bytes!(concat!(env!("OUT_DIR"), "/raytracing_test.rchit.spv"))
}

fn make_test_kernel(device: &Arc<HostVulkanDevice>, label: &str) -> Result<VulkanRayTracingKernel> {
    let stages = [
        RayTracingStage::ray_gen(rt_test_rgen_spv()),
        RayTracingStage::miss(rt_test_rmiss_spv()),
        RayTracingStage::closest_hit(rt_test_rchit_spv()),
    ];
    let groups = [
        RayTracingShaderGroup::General { general: 0 },
        RayTracingShaderGroup::General { general: 1 },
        RayTracingShaderGroup::TrianglesHit {
            closest_hit: Some(2),
            any_hit: None,
        },
    ];
    let bindings = [
        RayTracingBindingSpec::acceleration_structure(0, RayTracingShaderStageFlags::RAYGEN),
        RayTracingBindingSpec::storage_image(1, RayTracingShaderStageFlags::RAYGEN),
    ];
    VulkanRayTracingKernel::new(
        device,
        &RayTracingKernelDescriptor {
            label,
            stages: &stages,
            groups: &groups,
            bindings: &bindings,
            push_constants: RayTracingPushConstants::NONE,
            max_recursion_depth: 1,
        },
    )
}

fn binding_validation_descriptor<'a>(
    stages: &'a [RayTracingStage<'a>],
    groups: &'a [RayTracingShaderGroup],
    bindings: &'a [RayTracingBindingSpec],
) -> RayTracingKernelDescriptor<'a> {
    RayTracingKernelDescriptor {
        label: "rt-binding-validation",
        stages,
        groups,
        bindings,
        push_constants: RayTracingPushConstants::NONE,
        max_recursion_depth: 1,
    }
}

#[test]
fn rejects_one_slot_the_two_stages_spell_differently() {
    // The miss and closest-hit shaders declare no binding, so the second
    // spelling of slot 0 has to come from a rewrite of the ray-gen blob.
    let respelled = rename_binding_in_spirv_module(rt_test_rgen_spv(), "topLevelAS", "sceneTlas");
    let stages = [
        RayTracingStage::ray_gen(rt_test_rgen_spv()),
        RayTracingStage::miss(&respelled),
    ];
    let groups = [RayTracingShaderGroup::General { general: 0 }];
    let bindings = [
        RayTracingBindingSpec::acceleration_structure(0, RayTracingShaderStageFlags::RAYGEN),
        RayTracingBindingSpec::storage_image(1, RayTracingShaderStageFlags::RAYGEN),
    ];
    let err = validate_bindings_against_spirv(
        &binding_validation_descriptor(&stages, &groups, &bindings),
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("one slot cannot carry two names");
    let msg = format!("{err}");
    assert!(
        msg.contains("binding 0 is named `topLevelAS`")
            && msg.contains("`sceneTlas`")
            && msg.contains("one slot spelled two ways"),
        "expected both spellings of slot 0, got: {msg}"
    );
}

#[test]
fn a_stage_whose_only_set_is_not_set_0_is_refused_at_validation() {
    let moved_to_set_1 =
        move_binding_to_another_descriptor_set_in_spirv_module(rt_test_rgen_spv(), 0, 1);
    let stages = [RayTracingStage::ray_gen(&moved_to_set_1)];
    let groups = [RayTracingShaderGroup::General { general: 0 }];
    let bindings = [
        RayTracingBindingSpec::acceleration_structure(0, RayTracingShaderStageFlags::RAYGEN),
        RayTracingBindingSpec::storage_image(1, RayTracingShaderStageFlags::RAYGEN),
    ];
    let refusal = validate_bindings_against_spirv(
        &binding_validation_descriptor(&stages, &groups, &bindings),
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("a binding outside set 0 cannot be bound, so it cannot be dropped in silence");
    let message = format!("{refusal}");
    assert!(
        message.contains("only descriptor set 0 is supported") && message.contains('1'),
        "the refusal must name the unsupported set: {message}"
    );
}

#[test]
fn rejects_a_name_stripped_stage() {
    let stripped = strip_every_debug_name_from_spirv_module(rt_test_rgen_spv());
    let stages = [RayTracingStage::ray_gen(&stripped)];
    let groups = [RayTracingShaderGroup::General { general: 0 }];
    let bindings = [
        RayTracingBindingSpec::acceleration_structure(0, RayTracingShaderStageFlags::RAYGEN),
        RayTracingBindingSpec::storage_image(1, RayTracingShaderStageFlags::RAYGEN),
    ];
    let err = validate_bindings_against_spirv(
        &binding_validation_descriptor(&stages, &groups, &bindings),
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("a name-stripped blob cannot be bound by name");
    let msg = format!("{err}");
    assert!(
        msg.contains("carries no name") && msg.contains("Slang source"),
        "the refusal must name the cause and the fix, got: {msg}"
    );
}

#[test]
fn rejects_one_name_the_shader_puts_on_two_slots() {
    let collided = rename_binding_in_spirv_module(rt_test_rgen_spv(), "outputImage", "topLevelAS");
    let stages = [RayTracingStage::ray_gen(&collided)];
    let groups = [RayTracingShaderGroup::General { general: 0 }];
    let bindings = [
        RayTracingBindingSpec::acceleration_structure(0, RayTracingShaderStageFlags::RAYGEN),
        RayTracingBindingSpec::storage_image(1, RayTracingShaderStageFlags::RAYGEN),
    ];
    let err = validate_bindings_against_spirv(
        &binding_validation_descriptor(&stages, &groups, &bindings),
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("one name cannot identify two slots");
    let msg = format!("{err}");
    assert!(
        msg.contains("bindings 0 and 1 are both named `topLevelAS`")
            && msg.contains("one name on two slots"),
        "expected both slots and the name, got: {msg}"
    );
}

#[test]
fn rejects_a_declared_name_the_shader_spells_differently() {
    let stages = [RayTracingStage::ray_gen(rt_test_rgen_spv())];
    let groups = [RayTracingShaderGroup::General { general: 0 }];
    let bindings = [
        RayTracingBindingSpec::acceleration_structure(0, RayTracingShaderStageFlags::RAYGEN)
            .with_name("sceneTlas"),
        RayTracingBindingSpec::storage_image(1, RayTracingShaderStageFlags::RAYGEN),
    ];
    let err = validate_bindings_against_spirv(
        &binding_validation_descriptor(&stages, &groups, &bindings),
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("a declared name the shader does not use must be refused");
    let msg = format!("{err}");
    assert!(
        msg.contains("declared name `sceneTlas`") && msg.contains("SPIR-V names it `topLevelAS`"),
        "expected both the declared and the reflected name, got: {msg}"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn kernel_constructs_against_real_device() {
    let Some(device) = try_ray_tracing_device() else {
        return;
    };
    let _kernel = make_test_kernel(&device, "rt-construct").expect("kernel creation");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn kernel_rejects_missing_binding_in_descriptor() {
    let Some(device) = try_ray_tracing_device() else {
        return;
    };
    // Shader binds 0=AS + 1=storage image; omit binding 1.
    let stages = [
        RayTracingStage::ray_gen(rt_test_rgen_spv()),
        RayTracingStage::miss(rt_test_rmiss_spv()),
        RayTracingStage::closest_hit(rt_test_rchit_spv()),
    ];
    let groups = [
        RayTracingShaderGroup::General { general: 0 },
        RayTracingShaderGroup::General { general: 1 },
        RayTracingShaderGroup::TrianglesHit {
            closest_hit: Some(2),
            any_hit: None,
        },
    ];
    let bindings = [RayTracingBindingSpec::acceleration_structure(
        0,
        RayTracingShaderStageFlags::RAYGEN,
    )];
    let result = VulkanRayTracingKernel::new(
        &device,
        &RayTracingKernelDescriptor {
            label: "rt-missing-binding",
            stages: &stages,
            groups: &groups,
            bindings: &bindings,
            push_constants: RayTracingPushConstants::NONE,
            max_recursion_depth: 1,
        },
    );
    let err = result.expect_err("expected validation failure");
    assert!(
        format!("{err}").contains("binding 1"),
        "expected error about missing binding 1, got: {err}"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn kernel_rejects_kind_mismatch() {
    let Some(device) = try_ray_tracing_device() else {
        return;
    };
    let stages = [
        RayTracingStage::ray_gen(rt_test_rgen_spv()),
        RayTracingStage::miss(rt_test_rmiss_spv()),
        RayTracingStage::closest_hit(rt_test_rchit_spv()),
    ];
    let groups = [
        RayTracingShaderGroup::General { general: 0 },
        RayTracingShaderGroup::General { general: 1 },
        RayTracingShaderGroup::TrianglesHit {
            closest_hit: Some(2),
            any_hit: None,
        },
    ];
    // Declare binding 0 (which the shader has as AS) as a storage buffer.
    let bindings = [
        RayTracingBindingSpec::storage_buffer(0, RayTracingShaderStageFlags::RAYGEN),
        RayTracingBindingSpec::storage_image(1, RayTracingShaderStageFlags::RAYGEN),
    ];
    let result = VulkanRayTracingKernel::new(
        &device,
        &RayTracingKernelDescriptor {
            label: "rt-kind-mismatch",
            stages: &stages,
            groups: &groups,
            bindings: &bindings,
            push_constants: RayTracingPushConstants::NONE,
            max_recursion_depth: 1,
        },
    );
    let err = result.expect_err("expected validation failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("binding 0") && msg.contains("StorageBuffer"),
        "expected mismatch error mentioning binding 0 + StorageBuffer, got: {msg}"
    );
}

/// An index past the last vertex must be refused before the build, not
/// handed to the driver: the AS build reads `vertexData` through a buffer
/// device address that no robustness guarantee bounds, so a device that
/// accepts it reads out of bounds and reports nothing.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_blas_build_refuses_an_index_past_the_last_vertex() {
    let Some(device) = try_ray_tracing_device() else {
        return;
    };
    let vertices: [f32; 9] = [
        -0.6, -0.6, 0.5, //
        0.6, -0.6, 0.5, //
        0.0, 0.6, 0.5, //
    ];
    let refusal = VulkanAccelerationStructure::build_triangles_blas(
        &device,
        "rt-test-blas-out-of-range-index",
        &vertices,
        &[0, 1, 3],
    )
    .expect_err("index 3 over three vertices cannot be built in silence");
    let message = refusal.to_string();
    assert!(
        message.contains("index 3") && message.contains("outside the 3 supplied"),
        "the refusal must name the index and the vertex count: {message}"
    );
}

/// Build a 1-triangle BLAS, single-instance TLAS, and run trace-rays
/// against a 64×64 storage image. Reads the result back and checks
/// that the centre pixel is hit (barycentric color, mostly red) and
/// the corner pixels are miss (dark blue from rmiss).
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn trace_rays_produces_hit_and_miss_pixels() {
    let Some(device) = try_ray_tracing_device() else {
        return;
    };

    // Triangle in clip-space-ish coords: covers the centre of the
    // [-1,1]² launch window, leaves the corners uncovered.
    let vertices: [f32; 9] = [
        -0.6, -0.6, 0.5, //
        0.6, -0.6, 0.5, //
        0.0, 0.6, 0.5, //
    ];
    let indices: [u32; 3] = [0, 1, 2];
    let blas = VulkanAccelerationStructure::build_triangles_blas(
        &device,
        "rt-test-blas",
        &vertices,
        &indices,
    )
    .expect("BLAS build");
    assert_eq!(
        blas.kind(),
        crate::vulkan::rhi::AccelerationStructureKind::BottomLevel
    );
    assert!(
        blas.device_address() != 0,
        "BLAS device_address must be non-zero — got {:#x}",
        blas.device_address()
    );

    let instance = TlasInstanceDesc::identity(blas.clone());
    let tlas = VulkanAccelerationStructure::build_tlas(
        &device,
        "rt-test-tlas",
        std::slice::from_ref(&instance),
    )
    .expect("TLAS build");
    assert!(
        tlas.device_address() != 0,
        "TLAS device_address must be non-zero"
    );

    // Storage image (64×64 RGBA8) + bring it into GENERAL before the kernel binds.
    const W: u32 = 64;
    const H: u32 = 64;
    let texture = HostVulkanTexture::new_device_local(
        &device,
        &TextureDescriptor {
            label: Some("rt-test-output"),
            width: W,
            height: H,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC,
        },
    )
    .expect("texture creation");
    let stream_texture = Texture::from_vulkan(texture);
    let image = stream_texture.vulkan_inner().image().expect("image handle");
    HostVulkanTexture::transition_to_general(&device, image).expect("transition output to GENERAL");

    let kernel = make_test_kernel(&device, "rt-trace-test").expect("kernel creation");
    kernel
        .set_acceleration_structure(0, &tlas)
        .expect("set TLAS");
    kernel
        .set_storage_image(1, &stream_texture)
        .expect("set storage image");
    kernel.trace_rays(W, H, 1).expect("trace_rays");

    let readback = VulkanTextureReadback::new_into_stream_error(
        &device,
        &TextureReadbackDescriptor {
            label: "rt-test-readback",
            format: TextureFormat::Rgba8Unorm,
            width: W,
            height: H,
        },
    )
    .expect("readback construction");
    let ticket = readback
        .submit(&stream_texture, TextureSourceLayout::General)
        .expect("readback submit");
    let bytes = readback
        .wait_and_read(ticket, u64::MAX)
        .expect("readback wait_and_read");

    // Centre pixel — closest-hit shader writes barycentric color.
    // Triangle vertices arranged so the centre ray hits at barycentric
    // ≈ (0, 1/3, 2/3), which the rchit shader stores as
    // (red=0, green~85, blue~170) in RGBA8. The miss shader writes
    // a dark blue (13, 13, 51) — green is the discriminator: above
    // 50 means we hit, below 30 means we missed. The test was
    // originally too lenient (`any channel > 0`) — that passed even
    // when *every* ray missed (sky color is non-zero), so the test
    // didn't lock in the AS-actually-being-hit invariant.
    let centre_idx = ((H / 2) * W + (W / 2)) as usize * 4;
    let centre_g = bytes[centre_idx + 1];
    assert!(
        centre_g > 50,
        "centre pixel should be lit by closest-hit shader (green > 50 from barycentric blend), got rgb=({}, {}, {}). Likely the AS isn't being hit — every ray returned the miss-shader color.",
        bytes[centre_idx],
        bytes[centre_idx + 1],
        bytes[centre_idx + 2],
    );

    // Corner pixel — should be miss-shader's dark blue (~ 0.05, 0.05, 0.20).
    let corner = (bytes[0], bytes[1], bytes[2]);
    assert!(
        corner.2 > corner.0 && corner.2 > corner.1 && corner.2 < 100,
        "corner pixel should be dark blue from miss shader (~13,13,51); got rgb=({}, {}, {})",
        corner.0,
        corner.1,
        corner.2
    );
}
