// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Compute-kernel handler tests.
//!
//! The fixtures the child files share.

use super::super::handle_lifecycle::EscalateHandleRegistry;
use super::super::kernel_shader_stage_source::registered_shader_stage_source;
use super::super::run_frames_with_the_escalate_gate_held;
use super::linux_and_macos::{
    handle_register_compute_kernel, handle_run_compute_kernel, handle_run_compute_kernel_batch,
    plan_supplied_compute_bindings,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateComputeBindingKind, EscalateRequestRegisterComputeKernel,
    EscalateRequestRegisterComputeKernelBinding, EscalateRequestRunComputeKernel,
    EscalateRequestRunComputeKernelBatch, EscalateRequestRunComputeKernelBatchDispatch,
    EscalateRequestRunComputeKernelBinding,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseOk;
use crate::core::context::{
    BatchedComputeKernelDispatch, BatchedComputeKernelDispatchBinding, GpuContext,
    GpuContextLimitedAccess, SurfaceBoundKernelBindingResource, TexturePoolDescriptor,
    TextureRegistration,
};
use crate::core::rhi::{
    ComputeBindingSpec, ShaderCompilationTargetStage, SurfaceBoundKernelBindingKind, TextureFormat,
    TextureUsages,
};
use crate::host_rhi::HostTextureExt;

mod batched_compute_dispatch_layout_and_refusal_tests;
mod batched_compute_dispatch_ordering_and_cost_tests;
mod compute_kernel_registration_and_binding_plan_tests;
mod compute_kernel_release_tests;
mod concurrent_stream_dispatch_tests;
mod single_compute_dispatch_tests;
mod tensor_storage_buffer_dispatch_tests;

/// Compute is an always-present capability now, so there is no bridge
/// to install — only a device to have or not have.
fn make_gpu_sandbox_if_available() -> Option<GpuContextLimitedAccess> {
    GpuContext::init_for_platform_sync()
        .ok()
        .map(GpuContextLimitedAccess::new)
}

fn register_from_slang(source: &str, stage: &str) -> EscalateRequestRegisterComputeKernel {
    EscalateRequestRegisterComputeKernel {
        bindings: Vec::new(),
        push_constant_size: 0,
        request_id: "rid-slang".to_string(),
        source: source.to_string(),
        stage: stage.to_string(),
        entry_point: String::new(),
        spv_hex: String::new(),
    }
}

fn refusal_message(response: EscalateResponse) -> String {
    match response {
        EscalateResponse::Err(err) => err.message,
        other => panic!("expected Err, got {other:?}"),
    }
}

fn supplied(
    entries: &[(&str, EscalateComputeBindingKind, &str)],
) -> Vec<EscalateRequestRunComputeKernelBinding> {
    entries
        .iter()
        .map(
            |(name, kind, target_id)| EscalateRequestRunComputeKernelBinding {
                kind: *kind,
                name: (*name).to_string(),
                target_id: (*target_id).to_string(),
            },
        )
        .collect()
}

/// The SPIR-V for the pass the v1 wire could not express — one
/// sampled input, one storage output, deliberately different kinds.
const READ_ONE_WRITE_ANOTHER_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/test_read_one_write_another.spv"));

fn read_one_write_another_spv_hex() -> String {
    READ_ONE_WRITE_ANOTHER_SPV
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn register_read_one_write_another(sandbox: &GpuContextLimitedAccess) -> EscalateResponseOk {
    let response = handle_register_compute_kernel(
        sandbox,
        &EscalateHandleRegistry::default(),
        "reg".to_string(),
        EscalateRequestRegisterComputeKernel {
            entry_point: "".to_string(),
            source: "".to_string(),
            stage: "".to_string(),
            bindings: Vec::new(),
            push_constant_size: 4,
            request_id: "reg".to_string(),
            spv_hex: read_one_write_another_spv_hex(),
        },
    );
    match response {
        EscalateResponse::Ok(ok) => ok,
        other => panic!("registering the conformance kernel failed: {other:?}"),
    }
}

/// Pass 1 of the chain: every channel gains 40/255.
const BRIGHTEN_SLANG: &str = "\
[[vk::binding(0, 0)]] Sampler2D unbrightened_image;
[[vk::binding(1, 0)]] [format(\"rgba8\")] RWTexture2D<float4> brightened_image;
[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    float4 source = unbrightened_image.Load(int3(invocation.xy, 0));
    brightened_image[invocation.xy] = float4(source.rgb + 40.0 / 255.0, source.a);
}
";

/// Pass 2 of the chain: every channel doubles. Deliberately not
/// commutative with pass 1, so running them in the wrong order — or
/// running pass 2 against pass 1's *input* — lands on different pixels.
const DOUBLE_SLANG: &str = "\
[[vk::binding(0, 0)]] Sampler2D brightened_image;
[[vk::binding(1, 0)]] [format(\"rgba8\")] RWTexture2D<float4> doubled_image;
[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    float4 source = brightened_image.Load(int3(invocation.xy, 0));
    doubled_image[invocation.xy] = float4(source.rgb * 2.0, source.a);
}
";

/// 10,20,30 brightened by 40 is 50,60,70; doubled is 100,120,140.
const CHAIN_SEED_RGBA: [u8; 4] = [10, 20, 30, 255];
const CHAIN_BRIGHTENED_RGBA: [u8; 4] = [50, 60, 70, 255];
const CHAIN_DOUBLED_RGBA: [u8; 4] = [100, 120, 140, 255];

fn register_slang_kernel(sandbox: &GpuContextLimitedAccess, source: &str) -> String {
    let response = handle_register_compute_kernel(
        sandbox,
        &EscalateHandleRegistry::default(),
        "reg-chain".to_string(),
        register_from_slang(source, "compute"),
    );
    match response {
        EscalateResponse::Ok(ok) => ok.handle_id,
        other => panic!("registering a chain kernel failed: {other:?}"),
    }
}

fn batched_dispatch(
    kernel_id: &str,
    source_binding: (&str, &str),
    output_binding: (&str, &str),
) -> EscalateRequestRunComputeKernelBatchDispatch {
    EscalateRequestRunComputeKernelBatchDispatch {
        bindings: vec![
            EscalateRequestRunComputeKernelBinding {
                kind: EscalateComputeBindingKind::SampledTexture,
                name: source_binding.0.to_string(),
                target_id: source_binding.1.to_string(),
            },
            EscalateRequestRunComputeKernelBinding {
                kind: EscalateComputeBindingKind::StorageImage,
                name: output_binding.0.to_string(),
                target_id: output_binding.1.to_string(),
            },
        ],
        group_count_x: 8,
        group_count_y: 8,
        group_count_z: 1,
        kernel_id: kernel_id.to_string(),
        push_constants_hex: String::new(),
    }
}

/// The requested 64×64 textures registered under fixed ids, the first
/// seeded with [`CHAIN_SEED_RGBA`]. Returned held: dropping a pooled
/// handle hands its slot back, and the registration would then name a
/// recycled texture.
fn seeded_chain_textures<const TEXTURE_COUNT: usize>(
    sandbox: &GpuContextLimitedAccess,
    ids: [&str; TEXTURE_COUNT],
) -> Vec<crate::core::context::PooledTextureHandle> {
    sandbox
        .escalate(|full| {
            let desc = TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm).with_usage(
                TextureUsages::TEXTURE_BINDING
                    | TextureUsages::STORAGE_BINDING
                    | TextureUsages::COPY_SRC
                    | TextureUsages::COPY_DST,
            );
            let mut held = Vec::with_capacity(ids.len());
            for id in ids {
                let texture = full.host_inner().acquire_texture(&desc)?;
                full.register_texture(id, texture.texture().clone());
                held.push(texture);
            }

            let (_pool_id, seed_buffer) =
                full.acquire_pixel_buffer(64, 64, crate::core::rhi::PixelFormat::Rgba32)?;
            let plane = seed_buffer.buffer_ref().plane_base_address(0);
            unsafe {
                for pixel in 0..(64 * 64) {
                    std::ptr::copy_nonoverlapping(
                        CHAIN_SEED_RGBA.as_ptr(),
                        plane.add(pixel * 4),
                        4,
                    );
                }
            }
            full.copy_pixel_buffer_to_texture(&seed_buffer, held[0].texture(), ids[0], 64, 64)?;
            Ok(held)
        })
        .expect("the pooled textures, the first seeded")
}

/// Read a surface back, sourcing the readback barrier from the layout
/// the surface is actually tracked in.
///
/// Not a hardcoded `General`: a batch leaves a sampled binding in
/// SHADER_READ_ONLY_OPTIMAL, and a barrier whose `oldLayout` disagrees
/// with the image makes the contents undefined by spec — so a test
/// asserting on those pixels would be reading what no driver owes it.
fn read_back_rgba8(
    sandbox: &GpuContextLimitedAccess,
    surface_id: &str,
    texture: &crate::core::rhi::Texture,
    label: &str,
) -> Vec<u8> {
    use crate::core::rhi::TextureSourceLayout;
    sandbox
        .escalate(|full| {
            let resting_layout = full
                .resolve_texture_registration_by_surface_id(surface_id, None, 64, 64)?
                .current_layout();
            let source_layout = if resting_layout
                == streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL
            {
                TextureSourceLayout::ShaderReadOnly
            } else {
                TextureSourceLayout::General
            };
            let readback =
                full.create_texture_readback(label, 64, 64, TextureFormat::Rgba8Unorm)?;
            let ticket = readback.submit(texture, source_layout)?;
            Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
        })
        .expect("the texture reads back")
}

/// The layout a surface's registration is tracked in, read through a
/// fresh resolve at the chain tests' fixed 64×64 extent.
fn tracked_layout_of_surface(
    sandbox: &GpuContextLimitedAccess,
    surface_id: &str,
) -> streamlib_consumer_rhi::VulkanLayout {
    sandbox
        .escalate(|full| {
            Ok(full
                .resolve_texture_registration_by_surface_id(surface_id, None, 64, 64)?
                .current_layout())
        })
        .expect("the surface still resolves")
}

fn assert_every_pixel_is(pixels: &[u8], expected: [u8; 4], what: &str) {
    for (index, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(*pixel, expected, "pixel {index} of {what}");
    }
}
