// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Binding resources to a ray-tracing kernel's slots and writing them into its descriptor set.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{RayTracingBindingKind, RayTracingBindingSpec, Texture};
use crate::core::{Error, Result};

use crate::vulkan::rhi::VulkanAccelerationStructure;

use super::{BindingResource, PendingState, VulkanRayTracingKernelInner};

impl VulkanRayTracingKernelInner {
    /// Bind a top-level acceleration structure at `binding`. The slot must
    /// be declared as [`RayTracingBindingKind::AccelerationStructure`].
    pub(crate) fn set_acceleration_structure(
        &self,
        binding: u32,
        tlas: &VulkanAccelerationStructure,
    ) -> Result<()> {
        self.expect_kind(binding, RayTracingBindingKind::AccelerationStructure)?;
        if tlas.kind() != crate::vulkan::rhi::AccelerationStructureKind::TopLevel {
            return Err(Error::GpuError(format!(
                "Ray-tracing kernel '{}': binding {} expects a top-level AS, got {:?}",
                self.label,
                binding,
                tlas.kind()
            )));
        }
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::AccelerationStructure {
                handle: tlas.vk_handle(),
                _keep_alive: tlas.clone(),
            },
        );
        Ok(())
    }

    /// Bind a storage buffer. Accepts either
    /// [`crate::core::rhi::PixelBuffer`] or
    /// [`crate::core::rhi::StorageBuffer`] via
    /// [`crate::vulkan::rhi::VulkanStorageBindable`].
    pub(crate) fn set_storage_buffer(
        &self,
        binding: u32,
        buffer: &(impl crate::vulkan::rhi::VulkanStorageBindable + ?Sized),
    ) -> Result<()> {
        self.expect_kind(binding, RayTracingBindingKind::StorageBuffer)?;
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::Buffer {
                buffer: buffer.vk_buffer(),
                size: buffer.vk_buffer_size(),
            },
        );
        Ok(())
    }

    /// Bind a uniform buffer. Accepts only
    /// [`crate::core::rhi::UniformBuffer`] via
    /// [`crate::vulkan::rhi::VulkanUniformBindable`].
    pub(crate) fn set_uniform_buffer(
        &self,
        binding: u32,
        buffer: &(impl crate::vulkan::rhi::VulkanUniformBindable + ?Sized),
    ) -> Result<()> {
        self.expect_kind(binding, RayTracingBindingKind::UniformBuffer)?;
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::Buffer {
                buffer: buffer.vk_buffer(),
                size: buffer.vk_buffer_size(),
            },
        );
        Ok(())
    }

    pub(crate) fn set_sampled_texture(&self, binding: u32, texture: &Texture) -> Result<()> {
        self.expect_kind(binding, RayTracingBindingKind::SampledTexture)?;
        let view = {
            use crate::host_rhi::HostTextureExt;
            texture.vulkan_inner().image_view()?
        };
        let sampler = self.default_sampler()?;
        self.pending
            .lock()
            .bindings
            .insert(binding, BindingResource::SampledImage { view, sampler });
        Ok(())
    }

    pub(crate) fn set_storage_image(&self, binding: u32, texture: &Texture) -> Result<()> {
        self.expect_kind(binding, RayTracingBindingKind::StorageImage)?;
        let view = {
            use crate::host_rhi::HostTextureExt;
            texture.vulkan_inner().image_view()?
        };
        self.pending
            .lock()
            .bindings
            .insert(binding, BindingResource::StorageImage { view });
        Ok(())
    }

    pub(crate) fn set_push_constants(&self, bytes: &[u8]) -> Result<()> {
        if bytes.len() as u32 != self.push_constant_size {
            return Err(Error::GpuError(format!(
                "Ray-tracing kernel '{}': push-constant size mismatch — got {} bytes, kernel declares {}",
                self.label,
                bytes.len(),
                self.push_constant_size
            )));
        }
        self.pending.lock().push_constants = bytes.to_vec();
        Ok(())
    }

    pub(crate) fn set_push_constants_value<T: Copy>(&self, value: &T) -> Result<()> {
        let size = std::mem::size_of::<T>();
        let bytes = unsafe { std::slice::from_raw_parts(value as *const T as *const u8, size) };
        self.set_push_constants(bytes)
    }

    pub(crate) fn bindings(&self) -> &[RayTracingBindingSpec] {
        &self.bindings
    }

    pub(crate) fn push_constant_size(&self) -> u32 {
        self.push_constant_size
    }

    fn expect_kind(&self, binding: u32, expected: RayTracingBindingKind) -> Result<()> {
        let spec = self
            .bindings
            .iter()
            .find(|b| b.binding == binding)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "Ray-tracing kernel '{}': binding {} not declared",
                    self.label, binding
                ))
            })?;
        if spec.kind != expected {
            return Err(Error::GpuError(format!(
                "Ray-tracing kernel '{}': binding {} declared as {:?}, but {:?} was set",
                self.label, binding, spec.kind, expected
            )));
        }
        Ok(())
    }

    fn default_sampler(&self) -> Result<vk::Sampler> {
        let mut guard = self.default_sampler.lock();
        if let Some(s) = *guard {
            return Ok(s);
        }
        let info = vk::SamplerCreateInfo::builder()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .min_lod(0.0)
            .max_lod(0.0)
            .border_color(vk::BorderColor::FLOAT_TRANSPARENT_BLACK)
            .unnormalized_coordinates(false)
            .build();
        let sampler = unsafe { self.device.create_sampler(&info, None) }.map_err(|e| {
            Error::GpuError(format!(
                "Ray-tracing kernel '{}': failed to create default sampler: {e}",
                self.label
            ))
        })?;
        *guard = Some(sampler);
        Ok(sampler)
    }

    pub(super) fn flush_descriptor_writes(&self, pending: &PendingState) -> Result<()> {
        let mut buffer_infos: Vec<vk::DescriptorBufferInfo> =
            Vec::with_capacity(self.bindings.len());
        let mut image_infos: Vec<vk::DescriptorImageInfo> = Vec::with_capacity(self.bindings.len());
        let mut as_handles: Vec<vk::AccelerationStructureKHR> =
            Vec::with_capacity(self.bindings.len());
        let mut as_writes: Vec<vk::WriteDescriptorSetAccelerationStructureKHR> =
            Vec::with_capacity(self.bindings.len());

        struct Slot {
            binding: u32,
            ty: vk::DescriptorType,
            buffer_idx: Option<usize>,
            image_idx: Option<usize>,
            as_idx: Option<usize>,
        }
        let mut slots: Vec<Slot> = Vec::with_capacity(self.bindings.len());

        for spec in &self.bindings {
            let res = pending.bindings.get(&spec.binding).expect("checked above");
            match (spec.kind, res) {
                (
                    RayTracingBindingKind::StorageBuffer,
                    BindingResource::Buffer { buffer, size },
                ) => {
                    let idx = buffer_infos.len();
                    buffer_infos.push(
                        vk::DescriptorBufferInfo::builder()
                            .buffer(*buffer)
                            .offset(0)
                            .range(*size)
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::STORAGE_BUFFER,
                        buffer_idx: Some(idx),
                        image_idx: None,
                        as_idx: None,
                    });
                }
                (
                    RayTracingBindingKind::UniformBuffer,
                    BindingResource::Buffer { buffer, size },
                ) => {
                    let idx = buffer_infos.len();
                    buffer_infos.push(
                        vk::DescriptorBufferInfo::builder()
                            .buffer(*buffer)
                            .offset(0)
                            .range(*size)
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::UNIFORM_BUFFER,
                        buffer_idx: Some(idx),
                        image_idx: None,
                        as_idx: None,
                    });
                }
                (
                    RayTracingBindingKind::SampledTexture,
                    BindingResource::SampledImage { view, sampler },
                ) => {
                    let idx = image_infos.len();
                    image_infos.push(
                        vk::DescriptorImageInfo::builder()
                            .sampler(*sampler)
                            .image_view(*view)
                            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                        buffer_idx: None,
                        image_idx: Some(idx),
                        as_idx: None,
                    });
                }
                (RayTracingBindingKind::StorageImage, BindingResource::StorageImage { view }) => {
                    let idx = image_infos.len();
                    image_infos.push(
                        vk::DescriptorImageInfo::builder()
                            .image_view(*view)
                            .image_layout(vk::ImageLayout::GENERAL)
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::STORAGE_IMAGE,
                        buffer_idx: None,
                        image_idx: Some(idx),
                        as_idx: None,
                    });
                }
                (
                    RayTracingBindingKind::AccelerationStructure,
                    BindingResource::AccelerationStructure { handle, .. },
                ) => {
                    let idx = as_handles.len();
                    as_handles.push(*handle);
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::ACCELERATION_STRUCTURE_KHR,
                        buffer_idx: None,
                        image_idx: None,
                        as_idx: Some(idx),
                    });
                }
                _ => {
                    return Err(Error::GpuError(format!(
                        "Ray-tracing kernel '{}': binding {} kind/resource mismatch (declared {:?})",
                        self.label, spec.binding, spec.kind
                    )));
                }
            }
        }

        // Pre-build the AS-write extension structs (they reference into
        // `as_handles` which must outlive the update_descriptor_sets call).
        for slot in &slots {
            if let Some(idx) = slot.as_idx {
                let as_write = vk::WriteDescriptorSetAccelerationStructureKHR::builder()
                    .acceleration_structures(std::slice::from_ref(&as_handles[idx]))
                    .build();
                as_writes.push(as_write);
            }
        }

        let mut as_write_iter = as_writes.iter_mut();
        let mut writes: Vec<vk::WriteDescriptorSet> = Vec::with_capacity(slots.len());
        for slot in &slots {
            let mut write = vk::WriteDescriptorSet::builder()
                .dst_set(self.descriptor_set)
                .dst_binding(slot.binding)
                .descriptor_type(slot.ty);
            if let Some(i) = slot.buffer_idx {
                write = write.buffer_info(std::slice::from_ref(&buffer_infos[i]));
            }
            if let Some(i) = slot.image_idx {
                write = write.image_info(std::slice::from_ref(&image_infos[i]));
            }
            if slot.as_idx.is_some() {
                let as_write = as_write_iter
                    .next()
                    .expect("as_writes built in lockstep with as_idx slots");
                write = write.push_next(as_write);
            }
            // ACCELERATION_STRUCTURE_KHR descriptors carry the AS handle
            // through the chained `VkWriteDescriptorSetAccelerationStructureKHR`,
            // not through buffer_info / image_info — and the
            // `buffer_info` / `image_info` setters that normally set
            // `descriptor_count` aren't called for this shape, so the
            // count stays at the builder default (0). The driver
            // silently no-ops a write with `descriptorCount == 0`,
            // which means the AS binding never updates and every ray
            // returns the miss-shader output. Set it explicitly after
            // build — the builder exposes descriptor_count only as a
            // public field on the underlying struct.
            let mut built = write.build();
            if slot.as_idx.is_some() {
                built.descriptor_count = 1;
            }
            writes.push(built);
        }

        unsafe {
            self.device
                .update_descriptor_sets(&writes, &[] as &[vk::CopyDescriptorSet]);
        }
        Ok(())
    }
}
