// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Vulkan compute-kernel RHI: shader pipeline + descriptor set + dispatch primitives.
//!
//! Pattern (Granite-style): the kernel author declares the binding layout once
//! as data; the RHI reflects the SPIR-V at creation, validates the declaration
//! against the shader, and from that point on the user calls typed setters by
//! slot. `dispatch()` flushes pending descriptor writes, records the dispatch
//! command buffer, submits, and waits.
//!
//! Binding kinds supported today: storage buffer, uniform buffer, sampled
//! texture (with a default linear-clamp sampler), storage image. All bindings
//! live on descriptor set 0.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use std::ffi::c_void;

use crate::core::rhi::{ComputeBindingKind, ComputeBindingSpec, ComputeKernelDescriptor, Texture};
use crate::core::{Error, Result};

use crate::vulkan::rhi::{HostVulkanDevice, SampledImageReadLayout};

/// One compute kernel: shader pipeline + descriptor set + per-dispatch primitives.
///
/// `set_*` methods stage descriptor writes; `dispatch(x, y, z)` flushes them,
/// records the command buffer, submits, and waits for completion. Each kernel
/// holds a single descriptor set, so dispatches against it are serial — that's
/// fine for the format-converter / compositor / codec-pre-process workloads
/// this abstraction targets.
/// Rich data backing a [`VulkanComputeKernel`], reached through the
/// kernel's opaque handle.
pub(crate) struct VulkanComputeKernelInner {
    label: String,
    vulkan_device: Arc<HostVulkanDevice>,
    device: vulkanalia::Device,
    queue: vk::Queue,
    bindings: Vec<ComputeBindingSpec>,
    push_constant_size: u32,
    pipeline: vk::Pipeline,
    pipeline_layout: vk::PipelineLayout,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
    shader_module: vk::ShaderModule,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    /// Default sampler used for [`ComputeBindingKind::SampledTexture`] bindings.
    /// Created on-demand when the first sampled-texture binding is set.
    default_sampler: Mutex<Option<vk::Sampler>>,
    /// Binding indices that were declared with `pImmutableSamplers` at
    /// descriptor-set-layout creation time. Callers binding such a slot
    /// must use [`Self::set_combined_image_sampler_view`] (view-only) —
    /// the immutable sampler is baked into the layout and any sampler in
    /// the descriptor write is ignored. Empty when the kernel was built
    /// via [`Self::new`].
    immutable_sampler_bindings: Vec<u32>,
    pending: Mutex<PendingState>,
}

struct PendingState {
    bindings: HashMap<u32, BindingResource>,
    push_constants: Vec<u8>,
}

#[derive(Clone, Copy)]
enum BindingResource {
    Buffer {
        buffer: vk::Buffer,
        offset: vk::DeviceSize,
        range: vk::DeviceSize,
    },
    /// `COMBINED_IMAGE_SAMPLER` write — `sampler` may be `VK_NULL_HANDLE`
    /// when the descriptor-set layout slot uses an immutable sampler
    /// (per `pImmutableSamplers` at layout-creation time; the sampler
    /// field in the write is then ignored by the implementation).
    SampledImage {
        view: vk::ImageView,
        sampler: vk::Sampler,
    },
    /// `SAMPLED_IMAGE` write — image view only, no sampler. GLSL
    /// `texture2D` / `texelFetch` style.
    SampledImageOnly {
        view: vk::ImageView,
        layout: SampledImageReadLayout,
    },
    StorageImage {
        view: vk::ImageView,
    },
}

mod dispatch_and_recording;
mod kernel_construction;
mod pipeline_creation;
mod spirv_validation;

use pipeline_creation::vk_image_view_for;

impl VulkanComputeKernelInner {
    /// Bind a storage buffer at `binding`. The slot must be declared as
    /// [`ComputeBindingKind::StorageBuffer`] in the descriptor.
    ///
    /// Accepts either a [`crate::core::rhi::PixelBuffer`] (pixel-shaped
    /// data being bound as an SSBO — pixel buffers carry `STORAGE_BUFFER`
    /// usage from allocation time) or a
    /// [`crate::core::rhi::StorageBuffer`] (canonical raw-bytes shape
    /// from
    /// [`crate::core::context::GpuContext::acquire_storage_buffer`]).
    pub(crate) fn set_storage_buffer(
        &self,
        binding: u32,
        buffer: &(impl crate::vulkan::rhi::VulkanStorageBindable + ?Sized),
    ) -> Result<()> {
        self.set_storage_buffer_from_byte_offset(binding, buffer, 0)
    }

    /// Bind the part of a storage buffer from `byte_offset` to its end at
    /// `binding`, so the shader's byte 0 is the buffer's `byte_offset`.
    /// Refused unless `byte_offset` is a multiple of the device's
    /// `minStorageBufferOffsetAlignment` and inside the buffer.
    pub(crate) fn set_storage_buffer_from_byte_offset(
        &self,
        binding: u32,
        buffer: &(impl crate::vulkan::rhi::VulkanStorageBindable + ?Sized),
        byte_offset: vk::DeviceSize,
    ) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::StorageBuffer)?;
        let alignment = self.vulkan_device.min_storage_buffer_offset_alignment();
        if alignment != 0 && !byte_offset.is_multiple_of(alignment) {
            return Err(Error::Configuration(format!(
                "Compute kernel '{}': storage-buffer offset {byte_offset} at binding {binding} is \
                 not a multiple of the device's minStorageBufferOffsetAlignment {alignment}",
                self.label
            )));
        }
        let buffer_size = buffer.vk_buffer_size();
        if byte_offset >= buffer_size {
            return Err(Error::Configuration(format!(
                "Compute kernel '{}': storage-buffer offset {byte_offset} at binding {binding} is \
                 past the end of the {buffer_size}-byte buffer",
                self.label
            )));
        }
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::Buffer {
                buffer: buffer.vk_buffer(),
                offset: byte_offset,
                range: buffer_size - byte_offset,
            },
        );
        Ok(())
    }

    /// Bind a uniform buffer at `binding`. The slot must be declared as
    /// [`ComputeBindingKind::UniformBuffer`] in the descriptor.
    ///
    /// Accepts only [`crate::core::rhi::UniformBuffer`]. Pixel buffers
    /// cannot be bound as UBOs because their allocations don't carry
    /// `UNIFORM_BUFFER` usage; this is enforced at compile time via
    /// [`crate::vulkan::rhi::VulkanUniformBindable`].
    pub(crate) fn set_uniform_buffer(
        &self,
        binding: u32,
        buffer: &(impl crate::vulkan::rhi::VulkanUniformBindable + ?Sized),
    ) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::UniformBuffer)?;
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::Buffer {
                buffer: buffer.vk_buffer(),
                offset: 0,
                range: buffer.vk_buffer_size(),
            },
        );
        Ok(())
    }

    /// Bind a sampled texture at `binding`, using the kernel's default
    /// linear-clamp sampler. Errors if the binding was declared with
    /// an immutable sampler at construction time (in that case use
    /// [`Self::set_combined_image_sampler_view`] — Vulkan would
    /// silently ignore the default sampler in favor of the
    /// layout-baked one, and a caller passing a non-default sampler
    /// in `texture` would not get it applied).
    pub(crate) fn set_sampled_texture(&self, binding: u32, texture: &Texture) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::SampledTexture)?;
        if self.immutable_sampler_bindings.contains(&binding) {
            return Err(Error::GpuError(format!(
                "Compute kernel '{}': binding {} has an immutable sampler from \
                 the descriptor-set layout (`new_with_immutable_samplers`); \
                 use set_combined_image_sampler_view instead — the default \
                 sampler set_sampled_texture would supply is silently ignored \
                 by Vulkan when an immutable sampler is present",
                self.label, binding,
            )));
        }
        let view = vk_image_view_for(texture)?;
        let sampler = self.default_sampler()?;
        self.pending
            .lock()
            .bindings
            .insert(binding, BindingResource::SampledImage { view, sampler });
        Ok(())
    }

    /// Bind a storage image at `binding`. Caller is responsible for ensuring
    /// the texture's `STORAGE_BINDING` usage was set when it was created.
    pub(crate) fn set_storage_image(&self, binding: u32, texture: &Texture) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::StorageImage)?;
        let view = vk_image_view_for(texture)?;
        self.pending
            .lock()
            .bindings
            .insert(binding, BindingResource::StorageImage { view });
        Ok(())
    }

    /// Bind a raw `vk::ImageView` to a [`ComputeBindingKind::SampledImage`]
    /// slot (Vulkan `VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE` — GLSL `texture2D`).
    ///
    /// Escape hatch for callers that build per-plane reinterpreted-format
    /// views by hand against a
    /// multi-planar image — those views can't be expressed through the
    /// higher-level [`Self::set_sampled_texture`] / [`Self::set_storage_image`]
    /// setters that take engine `Texture` handles. `layout` is the layout the
    /// view is in at dispatch time, written into the descriptor.
    pub(crate) fn set_sampled_image_view(
        &self,
        binding: u32,
        view: vk::ImageView,
        layout: SampledImageReadLayout,
    ) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::SampledImage)?;
        self.pending
            .lock()
            .bindings
            .insert(binding, BindingResource::SampledImageOnly { view, layout });
        Ok(())
    }

    /// Bind a raw `vk::ImageView` to a [`ComputeBindingKind::StorageImage`]
    /// slot. Escape hatch for callers that build per-plane reinterpreted-
    /// format storage views by hand against a multi-planar image. The
    /// view must be in `GENERAL` layout at dispatch time.
    pub(crate) fn set_storage_image_view(&self, binding: u32, view: vk::ImageView) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::StorageImage)?;
        self.pending
            .lock()
            .bindings
            .insert(binding, BindingResource::StorageImage { view });
        Ok(())
    }

    /// Stage push constants for the next dispatch. Size must match the kernel's
    /// declared `push_constant_size` (and be 4-byte aligned per Vulkan).
    pub(crate) fn set_push_constants(&self, bytes: &[u8]) -> Result<()> {
        if bytes.len() as u32 != self.push_constant_size {
            return Err(Error::GpuError(format!(
                "Compute kernel '{}': push-constant size mismatch — got {} bytes, kernel declares {}",
                self.label,
                bytes.len(),
                self.push_constant_size
            )));
        }
        self.pending.lock().push_constants = bytes.to_vec();
        Ok(())
    }

    /// Convenience: stage a `Copy` value as push constants by reinterpreting its
    /// bytes. The value's size in bytes must match `push_constant_size`.
    pub(crate) fn set_push_constants_value<T: Copy>(&self, value: &T) -> Result<()> {
        let size = std::mem::size_of::<T>();
        let bytes = unsafe { std::slice::from_raw_parts(value as *const T as *const u8, size) };
        self.set_push_constants(bytes)
    }

    /// Bindings declared at construction time, in declaration order.
    pub(crate) fn bindings(&self) -> &[ComputeBindingSpec] {
        &self.bindings
    }

    /// Push-constant size in bytes (0 if the kernel has none).
    pub(crate) fn push_constant_size(&self) -> u32 {
        self.push_constant_size
    }

    fn expect_kind(&self, binding: u32, expected: ComputeBindingKind) -> Result<()> {
        let spec = self
            .bindings
            .iter()
            .find(|b| b.binding == binding)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "Compute kernel '{}': binding {} not declared",
                    self.label, binding
                ))
            })?;
        if spec.kind != expected {
            return Err(Error::GpuError(format!(
                "Compute kernel '{}': binding {} declared as {:?}, but {:?} was set",
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
        let sampler = unsafe { self.device.create_sampler(&info, None) }
            .map_err(|e| Error::GpuError(format!("Failed to create default sampler: {e}")))?;
        *guard = Some(sampler);
        Ok(sampler)
    }
}

impl Drop for VulkanComputeKernelInner {
    fn drop(&mut self) {
        unsafe {
            // Route through the queue-mutex-guarded helper, NOT raw
            // device_wait_idle — concurrent multi-processor setup makes raw
            // waits race queue submits (UNASSIGNED-Threading-Info → driver crash).
            let _ = self.vulkan_device.wait_idle();
            if let Some(sampler) = self.default_sampler.lock().take() {
                self.device.destroy_sampler(sampler, None);
            }
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device.destroy_pipeline(self.pipeline, None);
            self.device
                .destroy_pipeline_layout(self.pipeline_layout, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            self.device.destroy_shader_module(self.shader_module, None);
        }
    }
}

// Vulkan handles in this struct are protected by the kernel's owned fence:
// only one dispatch is in-flight at a time, and `dispatch()` blocks until
// the GPU completes. The `Mutex` around `pending` serializes setter writes
// across threads.
unsafe impl Send for VulkanComputeKernelInner {}
unsafe impl Sync for VulkanComputeKernelInner {}

impl std::fmt::Debug for VulkanComputeKernelInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanComputeKernelInner")
            .field("label", &self.label)
            .field("bindings", &self.bindings)
            .field("push_constant_size", &self.push_constant_size)
            .finish()
    }
}

// =============================================================================
// Public handle
// =============================================================================

/// Compute kernel — a layout-stable `#[repr(C)]` handle over an opaque
/// pointer to an `Arc<VulkanComputeKernelInner>`, plus cached POD
/// descriptors. `Clone` / `Drop` manage the inner Arc's strong count.
///
/// The `push_constant_size()` POD getter reads the cached field,
/// captured by [`Self::from_arc_into_raw`] at construction and never
/// mutated over the kernel's lifetime.
#[repr(C)]
pub struct VulkanComputeKernel {
    /// Opaque handle to the host's `Arc<VulkanComputeKernelInner>`.
    pub(crate) handle: *const c_void,
    /// Cached push-constant size in bytes. Set at construction; fixed
    /// for the kernel's lifetime.
    pub(crate) cached_push_constant_size: u32,
    /// Reserved padding so the struct stays 8-byte aligned and the
    /// trailing bytes of the last 4-byte field are deterministic.
    pub(crate) _reserved_padding: u32,
}

unsafe impl Send for VulkanComputeKernel {}
unsafe impl Sync for VulkanComputeKernel {}

impl VulkanComputeKernel {
    /// Create from a SPIR-V descriptor. Engine-side entry; mirrors
    /// `VulkanComputeKernelInner::new`.
    pub fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &ComputeKernelDescriptor<'_>,
    ) -> Result<Self> {
        let inner = VulkanComputeKernelInner::new(vulkan_device, descriptor)?;
        Ok(Self::from_arc_into_raw(Arc::new(inner)))
    }

    /// [`Self::new`], keeping the pipeline's on-disk cache in the stream's
    /// `pipeline_cache_directory_of_its_stream` when one is given.
    pub fn new_caching_its_pipeline_in(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &ComputeKernelDescriptor<'_>,
        pipeline_cache_directory_of_its_stream: Option<&Path>,
    ) -> Result<Self> {
        let inner = VulkanComputeKernelInner::new_caching_its_pipeline_in(
            vulkan_device,
            descriptor,
            pipeline_cache_directory_of_its_stream,
        )?;
        Ok(Self::from_arc_into_raw(Arc::new(inner)))
    }

    /// Create from a SPIR-V descriptor and a list of immutable samplers
    /// to bake into the descriptor-set layout. Mirrors
    /// [`VulkanComputeKernelInner::new_with_immutable_samplers`].
    pub fn new_with_immutable_samplers(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &ComputeKernelDescriptor<'_>,
        immutable_samplers: &[(u32, vk::Sampler)],
    ) -> Result<Self> {
        let inner = VulkanComputeKernelInner::new_with_immutable_samplers(
            vulkan_device,
            descriptor,
            immutable_samplers,
        )?;
        Ok(Self::from_arc_into_raw(Arc::new(inner)))
    }

    pub(crate) fn from_arc_into_raw(arc: Arc<VulkanComputeKernelInner>) -> Self {
        let cached_push_constant_size = arc.push_constant_size();
        let handle = Arc::into_raw(arc) as *const c_void;
        Self {
            handle,
            cached_push_constant_size,
            _reserved_padding: 0,
        }
    }

    pub(crate) fn host_inner(&self) -> &VulkanComputeKernelInner {
        unsafe { &*(self.handle as *const VulkanComputeKernelInner) }
    }

    /// Bind a [`crate::core::rhi::PixelBuffer`]-shaped storage buffer
    /// (SSBO) at `binding`. Pixel-shape SSBO is legitimate — pixel
    /// buffer allocations carry `STORAGE_BUFFER` usage from birth.
    ///
    /// Per-input-type concrete shape (no generic), without a kind
    /// discriminator. Mirrors the typed-binding pattern in Dawn / WebGPU
    /// (`wgpuComputePassEncoderSetBindGroup` family) and Unreal RHI
    /// (typed `SetShaderResourceViewParameter`).
    pub fn set_storage_buffer_pixel(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::PixelBuffer,
    ) -> Result<()> {
        self.host_inner().set_storage_buffer(binding, buffer)
    }

    /// Bind a raw-bytes [`crate::core::rhi::StorageBuffer`] at
    /// `binding` — the canonical shape from
    /// [`crate::core::context::GpuContext::acquire_storage_buffer`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn set_storage_buffer_storage(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::StorageBuffer,
    ) -> Result<()> {
        self.host_inner().set_storage_buffer(binding, buffer)
    }

    /// Bind a [`crate::core::rhi::StorageBuffer`] from `byte_offset` to its
    /// end at `binding`. See
    /// [`VulkanComputeKernelInner::set_storage_buffer_from_byte_offset`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn set_storage_buffer_storage_from_byte_offset(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::StorageBuffer,
        byte_offset: u64,
    ) -> Result<()> {
        self.host_inner()
            .set_storage_buffer_from_byte_offset(binding, buffer, byte_offset)
    }

    /// Bind a [`crate::core::rhi::PixelBuffer`] from `byte_offset` to its end
    /// at `binding`. See
    /// [`VulkanComputeKernelInner::set_storage_buffer_from_byte_offset`].
    pub(crate) fn set_storage_buffer_pixel_from_byte_offset(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::PixelBuffer,
        byte_offset: u64,
    ) -> Result<()> {
        self.host_inner()
            .set_storage_buffer_from_byte_offset(binding, buffer, byte_offset)
    }

    /// Bind a [`crate::core::rhi::UniformBuffer`] (UBO) at `binding`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn set_uniform_buffer(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::UniformBuffer,
    ) -> Result<()> {
        self.host_inner().set_uniform_buffer(binding, buffer)
    }

    pub fn set_sampled_texture(&self, binding: u32, texture: &Texture) -> Result<()> {
        self.host_inner().set_sampled_texture(binding, texture)
    }

    pub fn set_storage_image(&self, binding: u32, texture: &Texture) -> Result<()> {
        self.host_inner().set_storage_image(binding, texture)
    }

    /// Bind a raw `vk::ImageView` to a [`ComputeBindingKind::SampledImage`]
    /// slot. See [`VulkanComputeKernelInner::set_sampled_image_view`].
    pub fn set_sampled_image_view(
        &self,
        binding: u32,
        view: vk::ImageView,
        layout: SampledImageReadLayout,
    ) -> Result<()> {
        self.host_inner()
            .set_sampled_image_view(binding, view, layout)
    }

    /// Bind a raw `vk::ImageView` to a [`ComputeBindingKind::StorageImage`]
    /// slot. See [`VulkanComputeKernelInner::set_storage_image_view`].
    pub fn set_storage_image_view(&self, binding: u32, view: vk::ImageView) -> Result<()> {
        self.host_inner().set_storage_image_view(binding, view)
    }

    /// Upload push-constant bytes.
    pub fn set_push_constants(&self, bytes: &[u8]) -> Result<()> {
        self.host_inner().set_push_constants(bytes)
    }

    /// Convenience: re-interprets `&T` as a byte slice and forwards
    /// to [`Self::set_push_constants`].
    pub fn set_push_constants_value<T: Copy>(&self, value: &T) -> Result<()> {
        self.host_inner().set_push_constants_value(value)
    }

    /// Dispatch the kernel with the given workgroup counts.
    pub fn dispatch(
        &self,
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
    ) -> Result<()> {
        self.host_inner()
            .dispatch(group_count_x, group_count_y, group_count_z)
    }

    /// Record bind + push-constants + dispatch into a caller-owned
    /// command buffer. Crate-private surface; engine-internal callers
    /// only (`RgbToNv12Converter::convert`, `Nv12ToRgbConverter::convert`).
    pub(crate) fn record(
        &self,
        command_buffer: vk::CommandBuffer,
        group_x: u32,
        group_y: u32,
        group_z: u32,
    ) -> Result<()> {
        self.host_inner()
            .record(command_buffer, group_x, group_y, group_z)
    }

    /// Kernel's declared binding shape.
    pub fn bindings(&self) -> Vec<ComputeBindingSpec> {
        self.host_inner().bindings().to_vec()
    }

    /// Push-constant range size in bytes. Cached POD.
    pub fn push_constant_size(&self) -> u32 {
        self.cached_push_constant_size
    }
}

impl Clone for VulkanComputeKernel {
    fn clone(&self) -> Self {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<VulkanComputeKernelInner>)`;
            // the increment in `Clone` and this decrement are balanced.
            unsafe {
                Arc::increment_strong_count(self.handle as *const VulkanComputeKernelInner);
            }
        }
        Self {
            handle: self.handle,
            cached_push_constant_size: self.cached_push_constant_size,
            _reserved_padding: self._reserved_padding,
        }
    }
}

impl Drop for VulkanComputeKernel {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<VulkanComputeKernelInner>)`;
            // the increment in `Clone` and this decrement are balanced.
            unsafe {
                Arc::decrement_strong_count(self.handle as *const VulkanComputeKernelInner);
            }
        }
    }
}

impl std::fmt::Debug for VulkanComputeKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanComputeKernel").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;

    #[test]
    fn vulkan_compute_kernel_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<VulkanComputeKernel>();
    }
}

#[cfg(test)]
mod tests;
