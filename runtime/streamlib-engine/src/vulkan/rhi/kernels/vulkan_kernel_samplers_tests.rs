// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use vulkanalia::vk;

use super::vulkan_kernel_samplers::{
    SamplerOfABinding, refuse_samplers_the_kernel_cannot_take, vk_sampler_create_info,
};
use crate::core::Result;
use crate::core::rhi::{
    SamplerAddressMode, SamplerAnisotropy, SamplerDescriptor, SamplerMipmapMode,
};

fn anisotropic(anisotropy: SamplerAnisotropy) -> SamplerDescriptor {
    SamplerDescriptor {
        anisotropy,
        address_modes: [SamplerAddressMode::Repeat; 3],
        ..SamplerDescriptor::LINEAR_CLAMP
    }
}

fn refusal(binding: SamplerOfABinding, sampler_anisotropy_limit: u32) -> Result<()> {
    refuse_samplers_the_kernel_cannot_take(
        "Compute kernel 'k'",
        [binding],
        sampler_anisotropy_limit,
    )
}

#[test]
fn a_sampler_on_a_binding_that_samples_no_texture_is_refused() {
    let refused = refusal(
        SamplerOfABinding {
            binding: 2,
            samples_a_texture: false,
            sampler: Some(SamplerDescriptor::NEAREST_CLAMP),
        },
        16,
    )
    .expect_err("a storage binding takes no sampler")
    .to_string();
    assert!(
        refused.contains("binding 2") && refused.contains("samples no texture"),
        "{refused}"
    );
}

#[test]
fn anisotropy_beyond_the_device_limit_is_refused_and_within_it_is_taken() {
    let binding = |anisotropy| SamplerOfABinding {
        binding: 0,
        samples_a_texture: true,
        sampler: Some(anisotropic(anisotropy)),
    };
    let refused = refusal(binding(SamplerAnisotropy::X16), 8)
        .expect_err("16x is beyond an 8x device")
        .to_string();
    assert!(
        refused.contains("16x") && refused.contains("at most 8x"),
        "{refused}"
    );
    refusal(binding(SamplerAnisotropy::X8), 8).expect("8x is within an 8x device");
    refusal(binding(SamplerAnisotropy::Off), 1).expect("no anisotropy needs nothing");
}

#[test]
fn each_mipmap_mode_reaches_the_levels_it_names() {
    let lod_range = |mipmap_mode| {
        let info = vk_sampler_create_info(SamplerDescriptor {
            mipmap_mode,
            ..SamplerDescriptor::LINEAR_CLAMP
        });
        (info.mipmap_mode, info.max_lod)
    };
    assert_eq!(
        lod_range(SamplerMipmapMode::BaseLevelOnly),
        (vk::SamplerMipmapMode::NEAREST, 0.0)
    );
    assert_eq!(
        lod_range(SamplerMipmapMode::Linear),
        (vk::SamplerMipmapMode::LINEAR, vk::LOD_CLAMP_NONE)
    );
}
