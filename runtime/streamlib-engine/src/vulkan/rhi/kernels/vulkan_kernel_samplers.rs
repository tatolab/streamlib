// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The samplers a kernel's sampled-texture bindings read through, one per
//! distinct [`SamplerDescriptor`], and the refusal of one the kernel cannot
//! take.

use parking_lot::Mutex;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{
    SamplerAddressMode, SamplerBorderColor, SamplerDescriptor, SamplerFilter, SamplerMipmapMode,
};
use crate::core::{Error, Result};

/// One binding's sampler, as a kernel checks it at construction.
pub(crate) struct SamplerOfABinding {
    pub(crate) binding: u32,
    pub(crate) samples_a_texture: bool,
    pub(crate) sampler: Option<SamplerDescriptor>,
}

/// Refuse a sampler on a binding that samples no texture, and anisotropy
/// beyond what the device serves; `sampler_anisotropy_limit` is 1 on a device
/// without anisotropic filtering.
pub(crate) fn refuse_samplers_the_kernel_cannot_take(
    kernel_label: &str,
    bindings: impl IntoIterator<Item = SamplerOfABinding>,
    sampler_anisotropy_limit: u32,
) -> Result<()> {
    for SamplerOfABinding {
        binding,
        samples_a_texture,
        sampler,
    } in bindings
    {
        let Some(sampler) = sampler else {
            continue;
        };
        if !samples_a_texture {
            return Err(Error::GpuError(format!(
                "{kernel_label}: binding {binding} carries a sampler but samples no texture; \
                 only a sampled-texture binding takes one"
            )));
        }
        let samples = sampler.anisotropy.max_samples();
        if samples > sampler_anisotropy_limit {
            return Err(Error::GpuError(format!(
                "{kernel_label}: binding {binding} asks for {samples}x anisotropic filtering, \
                 and this device serves at most {sampler_anisotropy_limit}x"
            )));
        }
    }
    Ok(())
}

/// A kernel's samplers, created the first time a binding asks for each one.
pub(crate) struct VulkanKernelSamplers {
    device: vulkanalia::Device,
    /// Names the kernel in a refusal, as "Compute kernel 'label'".
    kernel_label: String,
    created: Mutex<Vec<(SamplerDescriptor, vk::Sampler)>>,
}

impl VulkanKernelSamplers {
    pub(crate) fn new(device: vulkanalia::Device, kernel_label: String) -> Self {
        Self {
            device,
            kernel_label,
            created: Mutex::new(Vec::new()),
        }
    }

    /// The sampler a binding declared, [`SamplerDescriptor::LINEAR_CLAMP`]
    /// when it declared none, created on first use.
    pub(crate) fn sampler_for(&self, declared: Option<SamplerDescriptor>) -> Result<vk::Sampler> {
        let descriptor = declared.unwrap_or_default();
        let mut created = self.created.lock();
        if let Some((_, sampler)) = created.iter().find(|(made, _)| *made == descriptor) {
            return Ok(*sampler);
        }
        let info = vk_sampler_create_info(descriptor);
        let sampler = unsafe { self.device.create_sampler(&info, None) }.map_err(|e| {
            Error::GpuError(format!(
                "{}: failed to create a sampler: {e}",
                self.kernel_label
            ))
        })?;
        created.push((descriptor, sampler));
        Ok(sampler)
    }

    /// Destroy every sampler created.
    ///
    /// # Safety
    /// No command buffer that reads one may still be pending.
    pub(crate) unsafe fn destroy(&mut self) {
        for (_, sampler) in self.created.get_mut().drain(..) {
            unsafe { self.device.destroy_sampler(sampler, None) };
        }
    }
}

/// The most anisotropic filtering `vulkan_device` serves, for tests that must
/// build an anisotropic sampler; the rig is asserted to serve some.
#[cfg(test)]
pub(crate) fn the_most_anisotropy_the_rig_serves(
    vulkan_device: &crate::vulkan::rhi::HostVulkanDevice,
) -> crate::core::rhi::SamplerAnisotropy {
    use crate::core::rhi::SamplerAnisotropy;
    let limit = vulkan_device.sampler_anisotropy_limit();
    [
        SamplerAnisotropy::X16,
        SamplerAnisotropy::X8,
        SamplerAnisotropy::X4,
        SamplerAnisotropy::X2,
    ]
    .into_iter()
    .find(|anisotropy| anisotropy.max_samples() <= limit)
    .expect("the rig must serve anisotropic filtering")
}

pub(super) fn vk_sampler_create_info(descriptor: SamplerDescriptor) -> vk::SamplerCreateInfo {
    let [address_mode_u, address_mode_v, address_mode_w] =
        descriptor.address_modes.map(vk_address_mode);
    let (mipmap_mode, max_lod) = match descriptor.mipmap_mode {
        SamplerMipmapMode::BaseLevelOnly => (vk::SamplerMipmapMode::NEAREST, 0.0),
        SamplerMipmapMode::Nearest => (vk::SamplerMipmapMode::NEAREST, vk::LOD_CLAMP_NONE),
        SamplerMipmapMode::Linear => (vk::SamplerMipmapMode::LINEAR, vk::LOD_CLAMP_NONE),
    };
    let max_anisotropy = descriptor.anisotropy.max_samples();
    vk::SamplerCreateInfo::builder()
        .mag_filter(vk_filter(descriptor.magnification_filter))
        .min_filter(vk_filter(descriptor.minification_filter))
        .mipmap_mode(mipmap_mode)
        .address_mode_u(address_mode_u)
        .address_mode_v(address_mode_v)
        .address_mode_w(address_mode_w)
        .anisotropy_enable(max_anisotropy > 1)
        .max_anisotropy(max_anisotropy as f32)
        .min_lod(0.0)
        .max_lod(max_lod)
        .border_color(vk_border_color(descriptor.border_color))
        .unnormalized_coordinates(false)
        .build()
}

fn vk_filter(filter: SamplerFilter) -> vk::Filter {
    match filter {
        SamplerFilter::Nearest => vk::Filter::NEAREST,
        SamplerFilter::Linear => vk::Filter::LINEAR,
    }
}

fn vk_address_mode(address_mode: SamplerAddressMode) -> vk::SamplerAddressMode {
    match address_mode {
        SamplerAddressMode::ClampToEdge => vk::SamplerAddressMode::CLAMP_TO_EDGE,
        SamplerAddressMode::Repeat => vk::SamplerAddressMode::REPEAT,
        SamplerAddressMode::MirroredRepeat => vk::SamplerAddressMode::MIRRORED_REPEAT,
        SamplerAddressMode::ClampToBorder => vk::SamplerAddressMode::CLAMP_TO_BORDER,
    }
}

fn vk_border_color(border_color: SamplerBorderColor) -> vk::BorderColor {
    match border_color {
        SamplerBorderColor::TransparentBlack => vk::BorderColor::FLOAT_TRANSPARENT_BLACK,
        SamplerBorderColor::OpaqueBlack => vk::BorderColor::FLOAT_OPAQUE_BLACK,
        SamplerBorderColor::OpaqueWhite => vk::BorderColor::FLOAT_OPAQUE_WHITE,
    }
}
