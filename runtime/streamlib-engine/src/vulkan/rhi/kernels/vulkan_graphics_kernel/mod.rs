// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Vulkan graphics-kernel RHI: multi-stage shader pipeline + descriptor-set
//! ring + per-frame draw primitives.
//!
//! Mirrors [`crate::vulkan::rhi::kernels::vulkan_compute_kernel::VulkanComputeKernel`] for graphics
//! pipelines: declare the stages and binding shape once, the kernel reflects
//! every stage's SPIR-V on construction, validates the merged declaration
//! against the shaders, and exposes typed setters by slot.
//!
//! A draw stages a descriptor-ring slot through the `set_*` setters, then
//! renders into offscreen color attachments with
//! [`VulkanGraphicsKernel::offscreen_render`], which owns its command buffer,
//! the render-pass scope and the submit. Layout transitions on inputs stay the
//! caller's.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use std::ffi::c_void;

use crate::core::rhi::{
    DrawCall, DrawIndexedCall, GraphicsBindingKind, GraphicsBindingSpec, GraphicsKernelDescriptor,
    GraphicsPipelineState, IndexType, Texture, VertexInputState,
};
use crate::core::{Error, Result};

use crate::vulkan::rhi::HostVulkanDevice;
use crate::vulkan::rhi::kernels::vulkan_kernel_samplers::VulkanKernelSamplers;
use pipeline_state_conversions::index_type_to_vk;

/// Rich data backing a [`VulkanGraphicsKernel`], reached through the
/// kernel's opaque handle.
pub(crate) struct VulkanGraphicsKernelInner {
    label: String,
    vulkan_device: Arc<HostVulkanDevice>,
    device: vulkanalia::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    bindings: Vec<GraphicsBindingSpec>,
    push_constant_size: u32,
    push_constant_stages: vk::ShaderStageFlags,
    pipeline_state: GraphicsPipelineState,
    descriptor_sets_in_flight: u32,
    pipeline: vk::Pipeline,
    pipeline_layout: vk::PipelineLayout,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    /// Ring of descriptor sets, one per `descriptor_sets_in_flight` slot.
    descriptor_sets: Vec<vk::DescriptorSet>,
    /// Per-slot pending state for staged setters; a draw flushes
    /// for the slot it's targeting.
    pending: Mutex<Vec<PendingState>>,
    /// The samplers [`GraphicsBindingKind::SampledTexture`] bindings read
    /// through, created when a binding is first set.
    samplers: VulkanKernelSamplers,
    /// Lazy offscreen scaffolding (cmd pool/buffer/fence) used by
    /// [`Self::offscreen_render`]; not allocated until first call.
    offscreen: Mutex<Option<OffscreenScaffold>>,
}

struct PendingState {
    bindings: HashMap<u32, BindingResource>,
    push_constants: Vec<u8>,
    vertex_buffers: HashMap<u32, BoundBuffer>,
    index_buffer: Option<BoundIndexBuffer>,
}

impl PendingState {
    fn new() -> Self {
        Self {
            bindings: HashMap::new(),
            push_constants: Vec::new(),
            vertex_buffers: HashMap::new(),
            index_buffer: None,
        }
    }
}

#[derive(Clone, Copy)]
enum BindingResource {
    Buffer {
        buffer: vk::Buffer,
        size: vk::DeviceSize,
    },
    SampledImage {
        view: vk::ImageView,
        sampler: vk::Sampler,
    },
    StorageImage {
        view: vk::ImageView,
    },
}

#[derive(Clone, Copy)]
struct BoundBuffer {
    buffer: vk::Buffer,
    offset: vk::DeviceSize,
}

#[derive(Clone, Copy)]
struct BoundIndexBuffer {
    buffer: vk::Buffer,
    offset: vk::DeviceSize,
    index_type: vk::IndexType,
}

struct OffscreenScaffold {
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
}

/// One color attachment for [`VulkanGraphicsKernel::offscreen_render`].
pub struct OffscreenColorTarget<'a> {
    pub texture: &'a Texture,
    /// `Some` clears the attachment to this RGBA value before drawing;
    /// `None` loads existing contents.
    pub clear_color: Option<[f32; 4]>,
}

/// Draw variant for [`VulkanGraphicsKernel::offscreen_render`].
pub enum OffscreenDraw {
    Draw(DrawCall),
    DrawIndexed(DrawIndexedCall),
}

mod kernel_construction;
mod offscreen_drawing;
mod pipeline_creation;
mod pipeline_state_conversions;
mod spirv_validation;

impl VulkanGraphicsKernelInner {
    /// Bindings declared at construction time.
    pub(crate) fn bindings(&self) -> &[GraphicsBindingSpec] {
        &self.bindings
    }

    /// Push-constant size in bytes (0 if the kernel has none).
    pub(crate) fn push_constant_size(&self) -> u32 {
        self.push_constant_size
    }

    /// Number of descriptor sets in the ring.
    pub(crate) fn descriptor_sets_in_flight(&self) -> u32 {
        self.descriptor_sets_in_flight
    }

    /// Bind a sampled texture at `(frame_index, binding)`, read through the
    /// binding's sampler.
    pub(crate) fn set_sampled_texture(
        &self,
        frame_index: u32,
        binding: u32,
        texture: &Texture,
    ) -> Result<()> {
        self.expect_kind(binding, GraphicsBindingKind::SampledTexture)?;
        use crate::host_rhi::HostTextureExt;
        let view = texture.vulkan_inner().image_view()?;
        let declared = self
            .bindings
            .iter()
            .find(|spec| spec.binding == binding)
            .and_then(|spec| spec.sampler);
        let sampler = self.samplers.sampler_for(declared)?;
        self.with_slot(frame_index, |slot| {
            slot.bindings
                .insert(binding, BindingResource::SampledImage { view, sampler });
            Ok(())
        })
    }

    /// Bind a storage buffer at `(frame_index, binding)`.
    ///
    /// Accepts either a [`crate::core::rhi::PixelBuffer`] or a
    /// [`crate::core::rhi::StorageBuffer`] via [`crate::vulkan::rhi::VulkanStorageBindable`].
    pub(crate) fn set_storage_buffer(
        &self,
        frame_index: u32,
        binding: u32,
        buffer: &(impl crate::vulkan::rhi::VulkanStorageBindable + ?Sized),
    ) -> Result<()> {
        self.expect_kind(binding, GraphicsBindingKind::StorageBuffer)?;
        let vk_buf = buffer.vk_buffer();
        let size = buffer.vk_buffer_size();
        self.with_slot(frame_index, |slot| {
            slot.bindings.insert(
                binding,
                BindingResource::Buffer {
                    buffer: vk_buf,
                    size,
                },
            );
            Ok(())
        })
    }

    /// Bind a uniform buffer at `(frame_index, binding)`.
    ///
    /// Accepts only [`crate::core::rhi::UniformBuffer`] via
    /// [`crate::vulkan::rhi::VulkanUniformBindable`].
    pub(crate) fn set_uniform_buffer(
        &self,
        frame_index: u32,
        binding: u32,
        buffer: &(impl crate::vulkan::rhi::VulkanUniformBindable + ?Sized),
    ) -> Result<()> {
        self.expect_kind(binding, GraphicsBindingKind::UniformBuffer)?;
        let vk_buf = buffer.vk_buffer();
        let size = buffer.vk_buffer_size();
        self.with_slot(frame_index, |slot| {
            slot.bindings.insert(
                binding,
                BindingResource::Buffer {
                    buffer: vk_buf,
                    size,
                },
            );
            Ok(())
        })
    }

    /// Bind a storage image at `(frame_index, binding)`.
    pub(crate) fn set_storage_image(
        &self,
        frame_index: u32,
        binding: u32,
        texture: &Texture,
    ) -> Result<()> {
        self.expect_kind(binding, GraphicsBindingKind::StorageImage)?;
        use crate::host_rhi::HostTextureExt;
        let view = texture.vulkan_inner().image_view()?;
        self.with_slot(frame_index, |slot| {
            slot.bindings
                .insert(binding, BindingResource::StorageImage { view });
            Ok(())
        })
    }

    /// Stage push constants for `frame_index`. Size must match the kernel's
    /// declared `push_constants.size`.
    pub(crate) fn set_push_constants(&self, frame_index: u32, bytes: &[u8]) -> Result<()> {
        if bytes.len() as u32 != self.push_constant_size {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{}': push-constant size mismatch — got {} bytes, kernel declares {}",
                self.label,
                bytes.len(),
                self.push_constant_size
            )));
        }
        self.with_slot(frame_index, |slot| {
            slot.push_constants = bytes.to_vec();
            Ok(())
        })
    }

    /// Convenience: stage a `Copy` value as push constants by reinterpreting
    /// its bytes. The value's size in bytes must match the declared push
    /// constant size.
    pub(crate) fn set_push_constants_value<T: Copy>(
        &self,
        frame_index: u32,
        value: &T,
    ) -> Result<()> {
        let size = std::mem::size_of::<T>();
        let bytes = unsafe { std::slice::from_raw_parts(value as *const T as *const u8, size) };
        self.set_push_constants(frame_index, bytes)
    }

    /// Bind a vertex buffer at `(frame_index, binding)`. `binding` must
    /// match a `VertexInputBinding` declared in the pipeline's vertex
    /// input state.
    ///
    /// Accepts only [`crate::core::rhi::VertexBuffer`] via
    /// [`crate::vulkan::rhi::VulkanVertexBindable`] — pixel buffers can't be bound as
    /// vertex input because their allocations don't carry `VERTEX_BUFFER`
    /// usage.
    pub(crate) fn set_vertex_buffer(
        &self,
        frame_index: u32,
        binding: u32,
        buffer: &(impl crate::vulkan::rhi::VulkanVertexBindable + ?Sized),
        offset: u64,
    ) -> Result<()> {
        match &self.pipeline_state.vertex_input {
            VertexInputState::None => {
                return Err(Error::GpuError(format!(
                    "Graphics kernel '{}': set_vertex_buffer called but pipeline has no vertex input bindings",
                    self.label
                )));
            }
            VertexInputState::Buffers { bindings, .. } => {
                if !bindings.iter().any(|b| b.binding == binding) {
                    return Err(Error::GpuError(format!(
                        "Graphics kernel '{}': vertex binding {binding} not declared in pipeline",
                        self.label
                    )));
                }
            }
        }
        let vk_buf = buffer.vk_buffer();
        self.with_slot(frame_index, |slot| {
            slot.vertex_buffers.insert(
                binding,
                BoundBuffer {
                    buffer: vk_buf,
                    offset,
                },
            );
            Ok(())
        })
    }

    /// Bind an index buffer at `frame_index`.
    ///
    /// Accepts only [`crate::core::rhi::IndexBuffer`] via
    /// [`crate::vulkan::rhi::VulkanIndexBindable`].
    pub(crate) fn set_index_buffer(
        &self,
        frame_index: u32,
        buffer: &(impl crate::vulkan::rhi::VulkanIndexBindable + ?Sized),
        offset: u64,
        index_type: IndexType,
    ) -> Result<()> {
        let vk_buf = buffer.vk_buffer();
        self.with_slot(frame_index, |slot| {
            slot.index_buffer = Some(BoundIndexBuffer {
                buffer: vk_buf,
                offset,
                index_type: index_type_to_vk(index_type),
            });
            Ok(())
        })
    }

    fn expect_kind(&self, binding: u32, expected: GraphicsBindingKind) -> Result<()> {
        let spec = self
            .bindings
            .iter()
            .find(|b| b.binding == binding)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "Graphics kernel '{}': binding {binding} not declared",
                    self.label
                ))
            })?;
        if spec.kind != expected {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{}': binding {binding} declared as {:?}, but {expected:?} was set",
                self.label, spec.kind
            )));
        }
        Ok(())
    }

    fn with_slot<R>(
        &self,
        frame_index: u32,
        f: impl FnOnce(&mut PendingState) -> Result<R>,
    ) -> Result<R> {
        if frame_index >= self.descriptor_sets_in_flight {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{}': frame_index {frame_index} out of range (ring depth {})",
                self.label, self.descriptor_sets_in_flight
            )));
        }
        let mut guard = self.pending.lock();
        f(&mut guard[frame_index as usize])
    }
}

impl Drop for VulkanGraphicsKernelInner {
    fn drop(&mut self) {
        unsafe {
            // Queue-mutex-guarded wait, not raw device_wait_idle (see
            // HostVulkanDevice::wait_idle — concurrent setup races otherwise).
            let _ = self.vulkan_device.wait_idle();
            if let Some(scaffold) = self.offscreen.lock().take() {
                self.device.destroy_fence(scaffold.fence, None);
                self.device
                    .destroy_command_pool(scaffold.command_pool, None);
            }
            self.samplers.destroy();
            self.device
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device.destroy_pipeline(self.pipeline, None);
            self.device
                .destroy_pipeline_layout(self.pipeline_layout, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_set_layout, None);
        }
    }
}

// Vulkan handles in this struct are protected at the granularity the API
// promises:
//   - `pipeline` / `pipeline_layout` / `descriptor_set_layout` are immutable
//     after construction.
//   - `descriptor_pool` is only updated through `update_descriptor_sets`,
//     which the spec permits as long as the set being updated isn't being
//     used by an in-flight command buffer (caller's responsibility — that's
//     why the ring is keyed by `frame_index`).
//   - `descriptor_sets[i]` is logically owned by frame slot `i`; serialized
//     across threads by `pending` mutex.
//   - `offscreen` scaffold is fence-protected for serial use.
unsafe impl Send for VulkanGraphicsKernelInner {}
unsafe impl Sync for VulkanGraphicsKernelInner {}

impl std::fmt::Debug for VulkanGraphicsKernelInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanGraphicsKernelInner")
            .field("label", &self.label)
            .field("bindings", &self.bindings)
            .field("push_constant_size", &self.push_constant_size)
            .field("descriptor_sets_in_flight", &self.descriptor_sets_in_flight)
            .finish()
    }
}

// =============================================================================
// Public handle
// =============================================================================

/// Graphics kernel — a layout-stable `#[repr(C)]` handle. Mirrors
/// `VulkanComputeKernel`'s shape: an opaque handle to the inner Arc plus
/// cached PODs.
#[repr(C)]
pub struct VulkanGraphicsKernel {
    /// Opaque handle to the host's `Arc<VulkanGraphicsKernelInner>`.
    pub(crate) handle: *const c_void,
    /// Cached push-constant size in bytes. Set at construction.
    pub(crate) cached_push_constant_size: u32,
    /// Cached descriptor-set ring depth. Set at construction.
    pub(crate) cached_descriptor_sets_in_flight: u32,
}

unsafe impl Send for VulkanGraphicsKernel {}
unsafe impl Sync for VulkanGraphicsKernel {}

impl VulkanGraphicsKernel {
    pub fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &GraphicsKernelDescriptor<'_>,
    ) -> Result<Self> {
        let inner = VulkanGraphicsKernelInner::new(vulkan_device, descriptor)?;
        Ok(Self::from_arc_into_raw(Arc::new(inner)))
    }

    /// [`Self::new`], keeping the pipeline's on-disk cache in the stream's
    /// `pipeline_cache_directory_of_its_stream` when one is given.
    pub fn new_caching_its_pipeline_in(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &GraphicsKernelDescriptor<'_>,
        pipeline_cache_directory_of_its_stream: Option<&Path>,
    ) -> Result<Self> {
        let inner = VulkanGraphicsKernelInner::new_caching_its_pipeline_in(
            vulkan_device,
            descriptor,
            pipeline_cache_directory_of_its_stream,
        )?;
        Ok(Self::from_arc_into_raw(Arc::new(inner)))
    }

    pub(crate) fn from_arc_into_raw(arc: Arc<VulkanGraphicsKernelInner>) -> Self {
        let cached_push_constant_size = arc.push_constant_size();
        let cached_descriptor_sets_in_flight = arc.descriptor_sets_in_flight();
        let handle = Arc::into_raw(arc) as *const c_void;
        Self {
            handle,
            cached_push_constant_size,
            cached_descriptor_sets_in_flight,
        }
    }

    pub(crate) fn host_inner(&self) -> &VulkanGraphicsKernelInner {
        unsafe { &*(self.handle as *const VulkanGraphicsKernelInner) }
    }

    /// Kernel's declared binding shape.
    pub fn bindings(&self) -> Vec<GraphicsBindingSpec> {
        self.host_inner().bindings().to_vec()
    }

    /// Push-constant range size in bytes. Cached POD.
    pub fn push_constant_size(&self) -> u32 {
        self.cached_push_constant_size
    }

    /// Descriptor-set ring depth. Cached POD.
    pub fn descriptor_sets_in_flight(&self) -> u32 {
        self.cached_descriptor_sets_in_flight
    }

    pub fn set_sampled_texture(
        &self,
        frame_index: u32,
        binding: u32,
        texture: &Texture,
    ) -> Result<()> {
        self.host_inner()
            .set_sampled_texture(frame_index, binding, texture)
    }

    /// Bind a [`crate::core::rhi::PixelBuffer`]-shaped storage
    /// buffer (SSBO) at `(frame_index, binding)`. Per-input-type
    /// concrete shape (no generic), without a kind discriminator.
    /// Mirrors `VulkanComputeKernel::set_storage_buffer_pixel`.
    pub fn set_storage_buffer_pixel(
        &self,
        frame_index: u32,
        binding: u32,
        buffer: &crate::core::rhi::PixelBuffer,
    ) -> Result<()> {
        self.host_inner()
            .set_storage_buffer(frame_index, binding, buffer)
    }

    /// Bind a raw-bytes [`crate::core::rhi::StorageBuffer`] at
    /// `(frame_index, binding)`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn set_storage_buffer_storage(
        &self,
        frame_index: u32,
        binding: u32,
        buffer: &crate::core::rhi::StorageBuffer,
    ) -> Result<()> {
        self.host_inner()
            .set_storage_buffer(frame_index, binding, buffer)
    }

    /// Bind a [`crate::core::rhi::UniformBuffer`] (UBO) at
    /// `(frame_index, binding)`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn set_uniform_buffer(
        &self,
        frame_index: u32,
        binding: u32,
        buffer: &crate::core::rhi::UniformBuffer,
    ) -> Result<()> {
        self.host_inner()
            .set_uniform_buffer(frame_index, binding, buffer)
    }

    pub fn set_storage_image(
        &self,
        frame_index: u32,
        binding: u32,
        texture: &Texture,
    ) -> Result<()> {
        self.host_inner()
            .set_storage_image(frame_index, binding, texture)
    }

    pub fn set_push_constants(&self, frame_index: u32, bytes: &[u8]) -> Result<()> {
        self.host_inner().set_push_constants(frame_index, bytes)
    }

    pub fn set_push_constants_value<T: Copy>(&self, frame_index: u32, value: &T) -> Result<()> {
        self.host_inner()
            .set_push_constants_value(frame_index, value)
    }

    /// Bind a [`crate::core::rhi::VertexBuffer`] at
    /// `(frame_index, binding)`. `binding` must match a
    /// `VertexInputBinding` declared in the pipeline's vertex input
    /// state.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn set_vertex_buffer(
        &self,
        frame_index: u32,
        binding: u32,
        buffer: &crate::core::rhi::VertexBuffer,
        offset: u64,
    ) -> Result<()> {
        self.host_inner()
            .set_vertex_buffer(frame_index, binding, buffer, offset)
    }

    /// Bind a [`crate::core::rhi::IndexBuffer`] at `frame_index`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn set_index_buffer(
        &self,
        frame_index: u32,
        buffer: &crate::core::rhi::IndexBuffer,
        offset: u64,
        index_type: IndexType,
    ) -> Result<()> {
        self.host_inner()
            .set_index_buffer(frame_index, buffer, offset, index_type)
    }

    /// Offscreen render. See [`VulkanGraphicsKernelInner::offscreen_render`].
    pub fn offscreen_render(
        &self,
        frame_index: u32,
        color_targets: &[crate::vulkan::rhi::OffscreenColorTarget<'_>],
        extent: (u32, u32),
        draw: crate::vulkan::rhi::OffscreenDraw,
    ) -> Result<()> {
        self.host_inner()
            .offscreen_render(frame_index, color_targets, extent, draw)
    }
}

impl Clone for VulkanGraphicsKernel {
    fn clone(&self) -> Self {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<VulkanGraphicsKernelInner>)`;
            // the increment in `Clone` and this decrement are balanced.
            unsafe {
                Arc::increment_strong_count(self.handle as *const VulkanGraphicsKernelInner);
            }
        }
        Self {
            handle: self.handle,
            cached_push_constant_size: self.cached_push_constant_size,
            cached_descriptor_sets_in_flight: self.cached_descriptor_sets_in_flight,
        }
    }
}

impl Drop for VulkanGraphicsKernel {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<VulkanGraphicsKernelInner>)`;
            // the increment in `Clone` and this decrement are balanced.
            unsafe {
                Arc::decrement_strong_count(self.handle as *const VulkanGraphicsKernelInner);
            }
        }
    }
}

impl std::fmt::Debug for VulkanGraphicsKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanGraphicsKernel").finish()
    }
}

#[cfg(test)]
mod tests;
