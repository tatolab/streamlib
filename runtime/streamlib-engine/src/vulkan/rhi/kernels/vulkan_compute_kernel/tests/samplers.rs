// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::rhi::{
    SamplerAddressMode, SamplerBorderColor, SamplerDescriptor, SamplerFilter, TextureDescriptor,
    TextureUsages,
};
use crate::host_rhi::HostTextureExt;
use crate::vulkan::rhi::HostVulkanTexture;

/// Writes the red channel, in 8-bit steps, of a sample halfway between the
/// texture's two texels and of one a quarter past its right edge.
const TWO_SAMPLES_SLANG: &str = r#"
[[vk::binding(0, 0)]] Sampler2D source;
[[vk::binding(1, 0)]] RWStructuredBuffer<uint> written;
[numthreads(1, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    written[0] = uint(round(source.SampleLevel(float2(0.5, 0.5), 0.0).r * 255.0));
    written[1] = uint(round(source.SampleLevel(float2(1.25, 0.5), 0.0).r * 255.0));
}
"#;

const LEFT_TEXEL_RED: u8 = 0;
const RIGHT_TEXEL_RED: u8 = 200;

/// A 2×1 RGBA texture: its left texel red 0, its right texel red 200.
fn two_texel_texture(device: &Arc<HostVulkanDevice>) -> Texture {
    let texels = [LEFT_TEXEL_RED, 0, 0, 255, RIGHT_TEXEL_RED, 0, 0, 255];
    let staging = HostVulkanBuffer::new_storage_buffer_host_visible(device, texels.len() as u64)
        .expect("staging buffer");
    unsafe {
        std::ptr::copy_nonoverlapping(texels.as_ptr(), staging.mapped_ptr(), texels.len());
    }
    let texture = HostVulkanTexture::new(
        device,
        &TextureDescriptor {
            width: 2,
            height: 1,
            format: crate::core::rhi::TextureFormat::Rgba8Unorm,
            usage: TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
            label: Some("two-texel-sampler-source"),
        },
    )
    .expect("source texture");
    let uploaded =
        unsafe { device.upload_buffer_to_image(staging.buffer(), &texture, 2, 1) }.expect("upload");
    assert_eq!(
        uploaded.final_texture_layout,
        crate::core::rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL
    );
    <Texture as HostTextureExt>::from_vulkan(texture)
}

/// A kernel running [`TWO_SAMPLES_SLANG`] over `bindings`.
fn two_samples_kernel(
    device: &Arc<HostVulkanDevice>,
    bindings: &[ComputeBindingSpec],
) -> crate::core::Result<VulkanComputeKernel> {
    let spv = crate::core::rhi::SlangShaderSourceCompiler::new()
        .compile_or_reuse(
            TWO_SAMPLES_SLANG,
            crate::core::rhi::ShaderCompilationTargetStage::Compute,
            "main",
            "two-samples",
        )
        .expect("the sampling shader compiles");
    VulkanComputeKernel::new(
        device,
        &ComputeKernelDescriptor {
            label: "two-samples",
            spv: &spv,
            entry_point: "main",
            bindings,
            push_constant_size: 0,
            specialization_constants: &[],
        },
    )
}

/// The two samples [`TWO_SAMPLES_SLANG`] takes through `sampler`.
fn samples_through(
    device: &Arc<HostVulkanDevice>,
    source: &Texture,
    sampler: Option<SamplerDescriptor>,
) -> Vec<u32> {
    let mut sampled = ComputeBindingSpec::sampled_texture(0);
    sampled.sampler = sampler;
    let kernel = two_samples_kernel(device, &[sampled, ComputeBindingSpec::storage_buffer(1)])
        .expect("the sampling kernel builds");
    let written = make_storage_buffer(device, 2);
    kernel
        .set_sampled_texture(0, source)
        .expect("the texture binds");
    kernel
        .set_storage_buffer_pixel(1, &written)
        .expect("the buffer binds");
    kernel.dispatch(1, 1, 1).expect("one invocation dispatches");
    read_buffer_u32(&written, 2)
}

fn sampling(filter: SamplerFilter, address_mode: SamplerAddressMode) -> SamplerDescriptor {
    SamplerDescriptor {
        magnification_filter: filter,
        minification_filter: filter,
        address_modes: [address_mode; 3],
        border_color: SamplerBorderColor::OpaqueWhite,
        ..SamplerDescriptor::LINEAR_CLAMP
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn each_binding_samples_through_the_filter_and_addressing_it_declares() {
    let device = vulkan_device_for_dispatch_tests();
    let source = two_texel_texture(&device);
    let halfway = (u32::from(LEFT_TEXEL_RED) + u32::from(RIGHT_TEXEL_RED)) / 2;
    let left = u32::from(LEFT_TEXEL_RED);
    let right = u32::from(RIGHT_TEXEL_RED);
    assert_eq!(
        samples_through(&device, &source, None),
        vec![halfway, right],
        "a binding without a sampler blends and clamps to the edge"
    );
    assert_eq!(
        samples_through(
            &device,
            &source,
            Some(sampling(SamplerFilter::Nearest, SamplerAddressMode::Repeat))
        ),
        vec![right, left],
        "unfiltered, a repeating coordinate a quarter past the edge reads the left texel"
    );
    assert_eq!(
        samples_through(
            &device,
            &source,
            Some(sampling(
                SamplerFilter::Nearest,
                SamplerAddressMode::ClampToBorder
            ))
        ),
        vec![right, 255],
        "past the edge a clamp-to-border sampler reads its opaque-white border"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn an_anisotropic_repeating_sampler_builds_and_samples() {
    let device = vulkan_device_for_dispatch_tests();
    let anisotropy =
        crate::vulkan::rhi::kernels::vulkan_kernel_samplers::the_most_anisotropy_the_rig_serves(
            &device,
        );
    let source = two_texel_texture(&device);
    let written = samples_through(
        &device,
        &source,
        Some(SamplerDescriptor {
            anisotropy,
            ..sampling(SamplerFilter::Linear, SamplerAddressMode::Repeat)
        }),
    );
    assert_eq!(written.len(), 2);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_sampler_on_a_storage_buffer_binding_is_refused() {
    let device = vulkan_device_for_dispatch_tests();
    let refusal = two_samples_kernel(
        &device,
        &[
            ComputeBindingSpec::sampled_texture(0),
            ComputeBindingSpec::storage_buffer(1).with_sampler(SamplerDescriptor::NEAREST_CLAMP),
        ],
    )
    .expect_err("a storage buffer takes no sampler")
    .to_string();
    assert!(refusal.contains("samples no texture"), "{refusal}");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_declared_sampler_on_a_binding_given_an_immutable_one_is_refused() {
    let device = vulkan_device_for_dispatch_tests();
    let spv = crate::core::rhi::SlangShaderSourceCompiler::new()
        .compile_or_reuse(
            TWO_SAMPLES_SLANG,
            crate::core::rhi::ShaderCompilationTargetStage::Compute,
            "main",
            "two-samples",
        )
        .expect("the sampling shader compiles");
    let bindings = [
        ComputeBindingSpec::sampled_texture(0).with_sampler(SamplerDescriptor::NEAREST_CLAMP),
        ComputeBindingSpec::storage_buffer(1),
    ];
    let refusal = VulkanComputeKernel::new_with_immutable_samplers(
        &device,
        &ComputeKernelDescriptor {
            label: "two-samples",
            spv: &spv,
            entry_point: "main",
            bindings: &bindings,
            push_constant_size: 0,
            specialization_constants: &[],
        },
        &[(0, vk::Sampler::null())],
    )
    .expect_err("two samplers for one binding must be refused")
    .to_string();
    assert!(refusal.contains("declare only one"), "{refusal}");
}
