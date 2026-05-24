// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use rspirv_reflect::{DescriptorType as RDescriptorType, Reflection};

use std::ffi::c_void;

use streamlib_plugin_abi::GpuContextFullAccessVTable;

use crate::core::rhi::{
    ComputeBindingKind, ComputeBindingSpec, ComputeKernelDescriptor, Texture,
};
use crate::core::{Result, Error};

/// Env var that overrides the default pipeline-cache directory. Used by tests
/// and headless / CI scenarios that need a writable, isolated cache root.
pub const PIPELINE_CACHE_DIR_ENV: &str = "STREAMLIB_PIPELINE_CACHE_DIR";

use super::HostVulkanDevice;

/// One compute kernel: shader pipeline + descriptor set + per-dispatch primitives.
///
/// `set_*` methods stage descriptor writes; `dispatch(x, y, z)` flushes them,
/// records the command buffer, submits, and waits for completion. Each kernel
/// holds a single descriptor set, so dispatches against it are serial — that's
/// fine for the format-converter / compositor / codec-pre-process workloads
/// this abstraction targets.
/// Host-only rich data backing a [`VulkanComputeKernel`]. Cdylib code
/// never sees this type; it reaches the public surface through the
/// `(handle, vtable)` β-shape.
pub struct VulkanComputeKernelInner {
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
        size: vk::DeviceSize,
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
    },
    StorageImage {
        view: vk::ImageView,
    },
}

impl VulkanComputeKernelInner {
    /// Create a new compute kernel from a SPIR-V shader and a binding declaration.
    ///
    /// Reflects the SPIR-V via `rspirv-reflect`, validates that the declared
    /// `bindings` match the shader's descriptor types, and rejects any mismatch
    /// before allocating Vulkan objects.
    pub fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &ComputeKernelDescriptor<'_>,
    ) -> Result<Self> {
        Self::new_inner(vulkan_device, descriptor, &[])
    }

    /// Variant of [`Self::new`] that bakes one or more immutable
    /// samplers into the descriptor-set layout, via `pImmutableSamplers`.
    ///
    /// Each `(binding, sampler)` pair attaches `sampler` to the matching
    /// declared binding at layout-creation time; the binding must be
    /// declared as [`ComputeBindingKind::SampledTexture`] (combined
    /// image-sampler, the only shape Vulkan permits an immutable sampler
    /// to bake into for compute today).
    ///
    /// **Lifetime contract.** The caller owns each `vk::Sampler` and is
    /// responsible for keeping it alive for the kernel's lifetime —
    /// the kernel records the handle in the layout but does not
    /// duplicate or refcount it. Dropping a sampler before the kernel
    /// is undefined behavior.
    ///
    /// Callers binding an immutable-sampler slot per-frame use
    /// [`Self::set_combined_image_sampler_view`], which writes the
    /// image view only — Vulkan ignores the sampler field in the write
    /// whenever the layout slot has an immutable sampler.
    pub fn new_with_immutable_samplers(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &ComputeKernelDescriptor<'_>,
        immutable_samplers: &[(u32, vk::Sampler)],
    ) -> Result<Self> {
        // Pre-validate: every (binding, sampler) pair must refer to a
        // declared SampledTexture binding. We catch this before going to
        // Vulkan so the error names the offending binding directly.
        for (binding, _) in immutable_samplers {
            let spec = descriptor
                .bindings
                .iter()
                .find(|b| b.binding == *binding)
                .ok_or_else(|| {
                    Error::GpuError(format!(
                        "Compute kernel '{}': immutable sampler refers to undeclared binding {}",
                        descriptor.label, binding
                    ))
                })?;
            if spec.kind != ComputeBindingKind::SampledTexture {
                return Err(Error::GpuError(format!(
                    "Compute kernel '{}': immutable sampler attached to binding {} \
                     declared as {:?}; only `SampledTexture` is supported",
                    descriptor.label, binding, spec.kind
                )));
            }
        }
        Self::new_inner(vulkan_device, descriptor, immutable_samplers)
    }

    fn new_inner(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &ComputeKernelDescriptor<'_>,
        immutable_samplers: &[(u32, vk::Sampler)],
    ) -> Result<Self> {
        let queue = vulkan_device.queue();
        let queue_family_index = vulkan_device.queue_family_index();
        let device = vulkan_device.device();

        validate_against_spirv(descriptor)?;

        let spirv: Vec<u32> = descriptor
            .spv
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        // ---- Vulkan objects ----------------------------------------------------------
        // Created in a strict order; on failure earlier objects are torn down by the
        // staged-cleanup helpers so we never leak on the error path.

        let shader_module = create_shader_module(device, &spirv, descriptor.label)?;

        let descriptor_set_layout = match create_descriptor_set_layout(
            device,
            descriptor.bindings,
            immutable_samplers,
        )
        {
            Ok(layout) => layout,
            Err(e) => {
                unsafe { device.destroy_shader_module(shader_module, None) };
                return Err(e);
            }
        };

        let pipeline_layout = match create_pipeline_layout(
            device,
            descriptor_set_layout,
            descriptor.push_constant_size,
        ) {
            Ok(layout) => layout,
            Err(e) => {
                unsafe {
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                }
                return Err(e);
            }
        };

        let pipeline = match create_compute_pipeline_with_cache(
            device,
            shader_module,
            pipeline_layout,
            descriptor.spv,
            descriptor.label,
        ) {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                }
                return Err(e);
            }
        };

        let descriptor_pool = match create_descriptor_pool(device, descriptor.bindings) {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                }
                return Err(e);
            }
        };

        let descriptor_set = match allocate_descriptor_set(
            device,
            descriptor_pool,
            descriptor_set_layout,
        ) {
            Ok(s) => s,
            Err(e) => {
                unsafe {
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                }
                return Err(e);
            }
        };

        let command_pool = match create_command_pool(device, queue_family_index) {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                }
                return Err(e);
            }
        };

        let command_buffer = match allocate_command_buffer(device, command_pool) {
            Ok(cb) => cb,
            Err(e) => {
                unsafe {
                    device.destroy_command_pool(command_pool, None);
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                }
                return Err(e);
            }
        };

        // Pre-signaled so the first dispatch can wait+reset without hanging.
        let fence_info = vk::FenceCreateInfo::builder()
            .flags(vk::FenceCreateFlags::SIGNALED)
            .build();
        let fence = match unsafe { device.create_fence(&fence_info, None) } {
            Ok(f) => f,
            Err(e) => {
                unsafe {
                    device.destroy_command_pool(command_pool, None);
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                }
                return Err(Error::GpuError(format!(
                    "Failed to create compute fence: {e}"
                )));
            }
        };

        Ok(Self {
            label: descriptor.label.to_string(),
            vulkan_device: Arc::clone(vulkan_device),
            device: device.clone(),
            queue,
            bindings: descriptor.bindings.to_vec(),
            push_constant_size: descriptor.push_constant_size,
            pipeline,
            pipeline_layout,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_set,
            shader_module,
            command_pool,
            command_buffer,
            fence,
            default_sampler: Mutex::new(None),
            immutable_sampler_bindings: immutable_samplers
                .iter()
                .map(|(b, _)| *b)
                .collect(),
            pending: Mutex::new(PendingState {
                bindings: HashMap::new(),
                push_constants: Vec::new(),
            }),
        })
    }

    /// Bind a storage buffer at `binding`. The slot must be declared as
    /// [`ComputeBindingKind::StorageBuffer`] in the descriptor.
    ///
    /// Accepts either a [`crate::core::rhi::PixelBuffer`] (pixel-shaped
    /// data being bound as an SSBO — pixel buffers carry `STORAGE_BUFFER`
    /// usage from allocation time) or a
    /// [`crate::core::rhi::StorageBuffer`] (canonical raw-bytes shape
    /// from
    /// [`crate::core::context::GpuContext::acquire_storage_buffer`]).
    pub fn set_storage_buffer(
        &self,
        binding: u32,
        buffer: &(impl super::VulkanStorageBindable + ?Sized),
    ) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::StorageBuffer)?;
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::Buffer {
                buffer: buffer.vk_buffer(),
                size: buffer.vk_buffer_size(),
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
    /// [`super::VulkanUniformBindable`].
    pub fn set_uniform_buffer(
        &self,
        binding: u32,
        buffer: &(impl super::VulkanUniformBindable + ?Sized),
    ) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::UniformBuffer)?;
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::Buffer {
                buffer: buffer.vk_buffer(),
                size: buffer.vk_buffer_size(),
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
    pub fn set_sampled_texture(&self, binding: u32, texture: &Texture) -> Result<()> {
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
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::SampledImage { view, sampler },
        );
        Ok(())
    }

    /// Bind a storage image at `binding`. Caller is responsible for ensuring
    /// the texture's `STORAGE_BINDING` usage was set when it was created.
    pub fn set_storage_image(&self, binding: u32, texture: &Texture) -> Result<()> {
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
    /// Escape hatch for callers (codec converters, video sessions) that
    /// build per-plane reinterpreted-format views by hand against a
    /// multi-planar image — those views can't be expressed through the
    /// higher-level [`Self::set_sampled_texture`] / [`Self::set_storage_image`]
    /// setters that take engine `Texture` handles. The view must be in
    /// `SHADER_READ_ONLY_OPTIMAL` at dispatch time; caller is responsible
    /// for the layout transition.
    pub(crate) fn set_sampled_image_view(
        &self,
        binding: u32,
        view: vk::ImageView,
    ) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::SampledImage)?;
        self.pending
            .lock()
            .bindings
            .insert(binding, BindingResource::SampledImageOnly { view });
        Ok(())
    }

    /// Bind a raw `vk::ImageView` to a [`ComputeBindingKind::SampledTexture`]
    /// slot (`VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER`) whose sampler
    /// is baked into the descriptor-set layout via `pImmutableSamplers`
    /// (see [`Self::new_with_immutable_samplers`]).
    ///
    /// The descriptor write supplies the view; the sampler field in the
    /// write is `VK_NULL_HANDLE` because Vulkan ignores it whenever the
    /// layout slot has an immutable sampler. The view must be in
    /// `SHADER_READ_ONLY_OPTIMAL` at dispatch time.
    ///
    /// Errors if the binding wasn't declared with an immutable sampler.
    pub(crate) fn set_combined_image_sampler_view(
        &self,
        binding: u32,
        view: vk::ImageView,
    ) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::SampledTexture)?;
        if !self.immutable_sampler_bindings.contains(&binding) {
            return Err(Error::GpuError(format!(
                "Compute kernel '{}': binding {} is SampledTexture but has no \
                 immutable sampler; use set_sampled_texture (engine Texture) \
                 or rebuild via new_with_immutable_samplers",
                self.label, binding
            )));
        }
        self.pending.lock().bindings.insert(
            binding,
            BindingResource::SampledImage {
                view,
                sampler: vk::Sampler::null(),
            },
        );
        Ok(())
    }

    /// Bind a raw `vk::ImageView` to a [`ComputeBindingKind::StorageImage`]
    /// slot. Escape hatch for callers that build per-plane reinterpreted-
    /// format storage views by hand against a multi-planar image. The
    /// view must be in `GENERAL` layout at dispatch time.
    pub(crate) fn set_storage_image_view(
        &self,
        binding: u32,
        view: vk::ImageView,
    ) -> Result<()> {
        self.expect_kind(binding, ComputeBindingKind::StorageImage)?;
        self.pending
            .lock()
            .bindings
            .insert(binding, BindingResource::StorageImage { view });
        Ok(())
    }

    /// Stage push constants for the next dispatch. Size must match the kernel's
    /// declared `push_constant_size` (and be 4-byte aligned per Vulkan).
    pub fn set_push_constants(&self, bytes: &[u8]) -> Result<()> {
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
    pub fn set_push_constants_value<T: Copy>(&self, value: &T) -> Result<()> {
        let size = std::mem::size_of::<T>();
        let bytes =
            unsafe { std::slice::from_raw_parts(value as *const T as *const u8, size) };
        self.set_push_constants(bytes)
    }

    /// Run the kernel: write all staged descriptors, record bind+push+dispatch,
    /// submit to the device queue, and wait on the kernel fence before returning.
    ///
    /// Every binding declared at construction must have been set since the last
    /// dispatch (unset bindings are an error — Vulkan's behavior in that case
    /// is undefined, so we refuse to dispatch).
    pub fn dispatch(&self, group_count_x: u32, group_count_y: u32, group_count_z: u32) -> Result<()> {
        // Drain + validate up-front so concurrent set_* calls during the
        // dispatch don't leak into the next one.
        let pending = self.drain_and_validate_pending()?;

        // Wait for prior dispatch (if any) to drain so the command buffer +
        // descriptor set are safe to mutate.
        unsafe {
            self.device
                .wait_for_fences(&[self.fence], true, u64::MAX)
                .map_err(|e| {
                    Error::GpuError(format!("Failed to wait for compute fence: {e}"))
                })?;
            self.device
                .reset_fences(&[self.fence])
                .map_err(|e| {
                    Error::GpuError(format!("Failed to reset compute fence: {e}"))
                })?;

            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| {
                    Error::GpuError(format!("Failed to reset command buffer: {e}"))
                })?;

            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build();
            self.device
                .begin_command_buffer(self.command_buffer, &begin_info)
                .map_err(|e| {
                    Error::GpuError(format!("Failed to begin command buffer: {e}"))
                })?;
        }

        self.record_inner(
            self.command_buffer,
            &pending,
            group_count_x,
            group_count_y,
            group_count_z,
        )?;

        unsafe {
            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| {
                    Error::GpuError(format!("Failed to end command buffer: {e}"))
                })?;

            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(self.command_buffer)
                .build();
            let cmd_infos = [cmd_info];
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .build();

            self.vulkan_device
                .submit_to_queue(self.queue, &[submit], self.fence)?;

            self.device
                .wait_for_fences(&[self.fence], true, u64::MAX)
                .map_err(|e| {
                    Error::GpuError(format!("Failed to wait for compute fence: {e}"))
                })?;
        }

        Ok(())
    }

    /// Record bind + push-constants + dispatch into a caller-owned
    /// command buffer (already in the `Recording` state via
    /// `vkBeginCommandBuffer`). Does **not** submit or wait; the caller
    /// is responsible for surrounding `end`/`submit`/sync.
    ///
    /// Drains the kernel's pending `set_*` state on entry, same as
    /// [`Self::dispatch`]. Validates every declared binding has been
    /// set; mismatches surface as [`Error::GpuError`] before any GPU
    /// recording happens.
    ///
    /// **Caller contract:** no concurrent `record` or `dispatch` on
    /// this kernel may be in flight — the kernel's descriptor set is
    /// shared across calls and Vulkan disallows updating an in-use
    /// descriptor set. For per-frame use, the recorder's own
    /// timeline-semaphore wait between frames satisfies this.
    pub(crate) fn record(
        &self,
        command_buffer: vk::CommandBuffer,
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
    ) -> Result<()> {
        let pending = self.drain_and_validate_pending()?;
        self.record_inner(
            command_buffer,
            &pending,
            group_count_x,
            group_count_y,
            group_count_z,
        )
    }

    fn drain_and_validate_pending(&self) -> Result<PendingState> {
        let pending = {
            let mut guard = self.pending.lock();
            PendingState {
                bindings: std::mem::take(&mut guard.bindings),
                push_constants: std::mem::take(&mut guard.push_constants),
            }
        };

        for spec in &self.bindings {
            if !pending.bindings.contains_key(&spec.binding) {
                return Err(Error::GpuError(format!(
                    "Compute kernel '{}': binding {} ({:?}) not set before dispatch",
                    self.label, spec.binding, spec.kind
                )));
            }
        }
        if self.push_constant_size > 0 && pending.push_constants.is_empty() {
            return Err(Error::GpuError(format!(
                "Compute kernel '{}': push constants not set before dispatch",
                self.label
            )));
        }

        Ok(pending)
    }

    fn record_inner(
        &self,
        command_buffer: vk::CommandBuffer,
        pending: &PendingState,
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
    ) -> Result<()> {
        self.flush_descriptor_writes(pending)?;

        unsafe {
            self.device.cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline,
            );

            self.device.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline_layout,
                0,
                &[self.descriptor_set],
                &[],
            );

            if self.push_constant_size > 0 {
                self.device.cmd_push_constants(
                    command_buffer,
                    self.pipeline_layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    &pending.push_constants,
                );
            }

            self.device.cmd_dispatch(
                command_buffer,
                group_count_x,
                group_count_y,
                group_count_z,
            );
        }

        Ok(())
    }

    /// Bindings declared at construction time, in declaration order.
    pub fn bindings(&self) -> &[ComputeBindingSpec] {
        &self.bindings
    }

    /// Push-constant size in bytes (0 if the kernel has none).
    pub fn push_constant_size(&self) -> u32 {
        self.push_constant_size
    }

    fn expect_kind(&self, binding: u32, expected: ComputeBindingKind) -> Result<()> {
        let spec = self.bindings.iter().find(|b| b.binding == binding).ok_or_else(|| {
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

    fn flush_descriptor_writes(&self, pending: &PendingState) -> Result<()> {
        let mut buffer_infos: Vec<vk::DescriptorBufferInfo> = Vec::with_capacity(self.bindings.len());
        let mut image_infos: Vec<vk::DescriptorImageInfo> = Vec::with_capacity(self.bindings.len());
        let mut writes: Vec<vk::WriteDescriptorSet> = Vec::with_capacity(self.bindings.len());

        // Build the *_info vectors first so all slot pointers are stable, then
        // build writes that reference them by index.
        struct Slot {
            binding: u32,
            ty: vk::DescriptorType,
            buffer_idx: Option<usize>,
            image_idx: Option<usize>,
        }
        let mut slots: Vec<Slot> = Vec::with_capacity(self.bindings.len());

        for spec in &self.bindings {
            let res = pending.bindings.get(&spec.binding).expect("checked above");
            match (spec.kind, res) {
                (ComputeBindingKind::StorageBuffer, BindingResource::Buffer { buffer, size }) => {
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
                    });
                }
                (ComputeBindingKind::UniformBuffer, BindingResource::Buffer { buffer, size }) => {
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
                    });
                }
                (
                    ComputeBindingKind::SampledTexture,
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
                    });
                }
                (
                    ComputeBindingKind::SampledImage,
                    BindingResource::SampledImageOnly { view },
                ) => {
                    let idx = image_infos.len();
                    image_infos.push(
                        vk::DescriptorImageInfo::builder()
                            .image_view(*view)
                            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::SAMPLED_IMAGE,
                        buffer_idx: None,
                        image_idx: Some(idx),
                    });
                }
                (ComputeBindingKind::StorageImage, BindingResource::StorageImage { view }) => {
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
                    });
                }
                _ => {
                    return Err(Error::GpuError(format!(
                        "Compute kernel '{}': binding {} kind/resource mismatch (declared {:?})",
                        self.label, spec.binding, spec.kind
                    )));
                }
            }
        }

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
            writes.push(write.build());
        }

        unsafe {
            self.device
                .update_descriptor_sets(&writes, &[] as &[vk::CopyDescriptorSet]);
        }
        Ok(())
    }
}

impl Drop for VulkanComputeKernelInner {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
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
// β-shape implementation (#917)
// =============================================================================

/// Compute kernel — layout-stable `#[repr(C)]` β-shape so cdylibs
/// can hold, refcount, drop, and read POD descriptors without
/// sharing rustc-version or dep-graph with the host.
///
/// The opaque handle points at an `Arc<VulkanComputeKernelInner>`;
/// lifecycle (Clone / Drop) dispatches through the host-installed
/// parent [`GpuContextFullAccessVTable`]'s `clone_compute_kernel` /
/// `drop_compute_kernel` callbacks (locked by PR #918's β-shape
/// Phase D work). Per-method dispatch is reached through the
/// dedicated
/// [`streamlib_plugin_abi::VulkanComputeKernelMethodsVTable`] pointed
/// at by `methods_vtable` — issue #907 PR 2/5 lands the pointer
/// plumbing + cached POD fields; follow-up PRs append method slots
/// and route `set_*` / `dispatch` / `record` / `bindings` through
/// them, plus land the ambitious CPU-reference dlopen integration
/// test.
///
/// The `push_constant_size()` POD getter reads from the cached
/// field — no FFI hop. The value is captured by
/// [`Self::from_arc_into_raw`] at construction and never mutates
/// over the kernel's lifetime.
#[repr(C)]
pub struct VulkanComputeKernel {
    /// Opaque handle to the host's `Arc<VulkanComputeKernelInner>`.
    pub(crate) handle: *const c_void,
    /// Parent vtable for cross-DSO Clone/Drop dispatch (#918 Phase D).
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Per-type vtable for cross-DSO method dispatch (#907 Phase E).
    pub(crate) methods_vtable:
        *const streamlib_plugin_abi::VulkanComputeKernelMethodsVTable,
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
        let vtable =
            crate::core::plugin::host_services::host_gpu_context_full_access_vtable();
        let methods_vtable =
            crate::core::plugin::host_services::host_vulkan_compute_kernel_methods_vtable();
        Self {
            handle,
            vtable,
            methods_vtable,
            cached_push_constant_size,
            _reserved_padding: 0,
        }
    }

    pub(crate) fn host_inner(&self) -> &VulkanComputeKernelInner {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "VulkanComputeKernel::host_inner() reached from cdylib code; \
                 this method must dispatch through the GpuContextFullAccessVTable."
            );
        }
        unsafe { &*(self.handle as *const VulkanComputeKernelInner) }
    }

    /// Bind a [`crate::core::rhi::PixelBuffer`]-shaped storage buffer
    /// (SSBO) at `binding`. Pixel-shape SSBO is legitimate — pixel
    /// buffer allocations carry `STORAGE_BUFFER` usage from birth.
    ///
    /// Per-input-type concrete shape (no generic) so the cdylib path
    /// can dispatch via the matching typed FFI slot
    /// (`set_storage_buffer_pixel`) without a kind discriminator.
    /// Mirrors the production cross-DSO pattern in Dawn / WebGPU
    /// (`wgpuComputePassEncoderSetBindGroup` family) and Unreal RHI
    /// (typed `SetShaderResourceViewParameter`).
    pub fn set_storage_buffer_pixel(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::PixelBuffer,
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_set_storage_buffer_pixel_via_vtable(binding, buffer);
        }
        self.host_inner().set_storage_buffer(binding, buffer)
    }

    /// Bind a raw-bytes [`crate::core::rhi::StorageBuffer`] at
    /// `binding` — the canonical shape from
    /// [`crate::core::context::GpuContext::acquire_storage_buffer`].
    #[cfg(target_os = "linux")]
    pub fn set_storage_buffer_storage(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::StorageBuffer,
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_set_storage_buffer_storage_via_vtable(binding, buffer);
        }
        self.host_inner().set_storage_buffer(binding, buffer)
    }

    /// Bind a [`crate::core::rhi::UniformBuffer`] (UBO) at `binding`.
    #[cfg(target_os = "linux")]
    pub fn set_uniform_buffer(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::UniformBuffer,
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_set_uniform_buffer_via_vtable(binding, buffer);
        }
        self.host_inner().set_uniform_buffer(binding, buffer)
    }

    pub fn set_sampled_texture(&self, binding: u32, texture: &Texture) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_set_sampled_texture_via_vtable(binding, texture);
        }
        self.host_inner().set_sampled_texture(binding, texture)
    }

    pub fn set_storage_image(&self, binding: u32, texture: &Texture) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_set_storage_image_via_vtable(binding, texture);
        }
        self.host_inner().set_storage_image(binding, texture)
    }

    /// Bind a raw `vk::ImageView` to a [`ComputeBindingKind::SampledImage`]
    /// slot. See [`VulkanComputeKernelInner::set_sampled_image_view`].
    ///
    /// Host-only: takes a raw `vk::ImageView` which cdylibs cannot construct
    /// (no `vulkanalia` dep). Crate-private surface; engine-internal callers
    /// only.
    pub(crate) fn set_sampled_image_view(
        &self,
        binding: u32,
        view: vk::ImageView,
    ) -> Result<()> {
        self.host_inner().set_sampled_image_view(binding, view)
    }

    /// Bind a raw `vk::ImageView` to a [`ComputeBindingKind::SampledTexture`]
    /// slot that was created with an immutable sampler. See
    /// [`VulkanComputeKernelInner::set_combined_image_sampler_view`].
    ///
    /// Host-only; see [`Self::set_sampled_image_view`] for the rationale.
    pub(crate) fn set_combined_image_sampler_view(
        &self,
        binding: u32,
        view: vk::ImageView,
    ) -> Result<()> {
        self.host_inner()
            .set_combined_image_sampler_view(binding, view)
    }

    /// Bind a raw `vk::ImageView` to a [`ComputeBindingKind::StorageImage`]
    /// slot. See [`VulkanComputeKernelInner::set_storage_image_view`].
    ///
    /// Host-only; see [`Self::set_sampled_image_view`] for the rationale.
    pub(crate) fn set_storage_image_view(
        &self,
        binding: u32,
        view: vk::ImageView,
    ) -> Result<()> {
        self.host_inner().set_storage_image_view(binding, view)
    }

    /// Upload push-constant bytes. In host mode dispatches directly
    /// through `host_inner`; in cdylib mode routes through the per-
    /// type methods vtable (#949 first slice).
    pub fn set_push_constants(&self, bytes: &[u8]) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_set_push_constants_via_vtable(bytes);
        }
        self.host_inner().set_push_constants(bytes)
    }

    /// Convenience: re-interprets `&T` as a byte slice and forwards
    /// to [`Self::set_push_constants`]. Inherits its dispatch
    /// contract — vtable in cdylib mode, host_inner otherwise.
    pub fn set_push_constants_value<T: Copy>(&self, value: &T) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            // SAFETY: T is Copy + Sized so its layout is stable; the
            // byte view is read-only and consumed inside the FFI call.
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    value as *const T as *const u8,
                    std::mem::size_of::<T>(),
                )
            };
            return self.dispatch_set_push_constants_via_vtable(bytes);
        }
        self.host_inner().set_push_constants_value(value)
    }

    /// Dispatch the kernel with the given workgroup counts. In host
    /// mode dispatches through `host_inner`; in cdylib mode routes
    /// through the per-type methods vtable (#949 first slice).
    pub fn dispatch(
        &self,
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
    ) -> Result<()> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_dispatch_via_vtable(
                group_count_x,
                group_count_y,
                group_count_z,
            );
        }
        self.host_inner()
            .dispatch(group_count_x, group_count_y, group_count_z)
    }

    #[cfg(target_os = "linux")]
    fn dispatch_set_push_constants_via_vtable(&self, bytes: &[u8]) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_push_constants: kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).set_push_constants)(
                self.handle,
                bytes.as_ptr(),
                bytes.len(),
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    #[cfg(target_os = "linux")]
    fn dispatch_dispatch_via_vtable(
        &self,
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "dispatch: kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).dispatch)(
                self.handle,
                group_count_x,
                group_count_y,
                group_count_z,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    #[cfg(target_os = "linux")]
    fn dispatch_set_storage_buffer_pixel_via_vtable(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::PixelBuffer,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_storage_buffer_pixel: kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).set_storage_buffer_pixel)(
                self.handle,
                binding,
                buffer.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    #[cfg(target_os = "linux")]
    fn dispatch_set_storage_buffer_storage_via_vtable(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::StorageBuffer,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_storage_buffer_storage: kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).set_storage_buffer_storage)(
                self.handle,
                binding,
                buffer.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    #[cfg(target_os = "linux")]
    fn dispatch_set_uniform_buffer_via_vtable(
        &self,
        binding: u32,
        buffer: &crate::core::rhi::UniformBuffer,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_uniform_buffer: kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).set_uniform_buffer)(
                self.handle,
                binding,
                buffer.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    #[cfg(target_os = "linux")]
    fn dispatch_set_sampled_texture_via_vtable(
        &self,
        binding: u32,
        texture: &Texture,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_sampled_texture: kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).set_sampled_texture)(
                self.handle,
                binding,
                texture.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    #[cfg(target_os = "linux")]
    fn dispatch_set_storage_image_via_vtable(
        &self,
        binding: u32,
        texture: &Texture,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_storage_image: kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).set_storage_image)(
                self.handle,
                binding,
                texture.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Record bind + push-constants + dispatch into a caller-owned
    /// command buffer. Host-only: takes a raw `vk::CommandBuffer`
    /// which cdylibs cannot construct (no `vulkanalia` dep). Crate-
    /// private surface; engine-internal callers only.
    pub(crate) fn record(
        &self,
        command_buffer: vk::CommandBuffer,
        group_x: u32,
        group_y: u32,
        group_z: u32,
    ) -> Result<()> {
        self.host_inner().record(command_buffer, group_x, group_y, group_z)
    }

    /// Kernel's declared binding shape. Mode-routed: host-mode reads
    /// directly from `host_inner`; cdylib-mode dispatches through the
    /// v4 `bindings` slot on the per-type methods vtable. On cdylib-
    /// side FFI errors (null vtable, malformed err_buf, host panic) the
    /// method emits a `tracing::warn` and returns an empty Vec — the
    /// public signature predates the introspection vtable and isn't
    /// fallible at the type level.
    pub fn bindings(&self) -> Vec<ComputeBindingSpec> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            return self.dispatch_bindings_via_vtable().unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    "VulkanComputeKernel::bindings cdylib dispatch failed; returning empty",
                );
                Vec::new()
            });
        }
        self.host_inner().bindings().to_vec()
    }

    #[cfg(target_os = "linux")]
    fn dispatch_bindings_via_vtable(&self) -> Result<Vec<ComputeBindingSpec>> {
        use crate::core::Error;
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "bindings: compute kernel methods vtable is null".into(),
            ));
        }
        // Inline-then-heap: one call with a stack buffer of cap=16
        // covers ~all real kernels (~8 bindings in practice) without
        // allocation. If the host signals status=2 (buffer-too-small),
        // fall back to a heap buffer sized to the host-reported
        // required count and call again.
        let mut buf = [streamlib_plugin_abi::ComputeBindingSpecRepr {
            binding: 0,
            kind: 0,
        }; 16];
        let mut out_len: usize = 0;
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.methods_vtable).bindings)(
                self.handle,
                buf.as_mut_ptr(),
                buf.len(),
                &mut out_len as *mut usize,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 2 {
            // Inline buffer too small — fall back to heap with the
            // host-reported required size.
            let mut heap: Vec<streamlib_plugin_abi::ComputeBindingSpecRepr> = vec![
                streamlib_plugin_abi::ComputeBindingSpecRepr {
                    binding: 0,
                    kind: 0,
                };
                out_len
            ];
            let mut out_len2: usize = 0;
            let status2 = unsafe {
                ((*self.methods_vtable).bindings)(
                    self.handle,
                    heap.as_mut_ptr(),
                    heap.len(),
                    &mut out_len2 as *mut usize,
                    err_buf.as_mut_ptr(),
                    err_buf.len(),
                    &mut err_len as *mut usize,
                )
            };
            if status2 != 0 {
                let msg =
                    String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                return Err(Error::GpuError(msg));
            }
            return heap
                .iter()
                .take(out_len2)
                .map(crate::core::rhi::plugin_abi_bridge::compute_binding_spec_from_repr)
                .collect();
        }
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            return Err(Error::GpuError(msg));
        }
        buf.iter()
            .take(out_len)
            .map(crate::core::rhi::plugin_abi_bridge::compute_binding_spec_from_repr)
            .collect()
    }

    /// Push-constant range size in bytes. Cached POD — no FFI hop.
    pub fn push_constant_size(&self) -> u32 {
        self.cached_push_constant_size
    }
}

impl Clone for VulkanComputeKernel {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            unsafe {
                ((*self.vtable).clone_compute_kernel)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            vtable: self.vtable,
            methods_vtable: self.methods_vtable,
            cached_push_constant_size: self.cached_push_constant_size,
            _reserved_padding: self._reserved_padding,
        }
    }
}

impl Drop for VulkanComputeKernel {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            unsafe {
                ((*self.vtable).drop_compute_kernel)(self.handle);
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
mod beta_shape_layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn vulkan_compute_kernel_layout() {
        // β-shape struct as of #907 PR 2/5:
        //   handle                       @ 0  (8 bytes, *const c_void)
        //   vtable                       @ 8  (8 bytes, *const GpuContextFullAccessVTable)
        //   methods_vtable               @ 16 (8 bytes, *const VulkanComputeKernelMethodsVTable)
        //   cached_push_constant_size    @ 24 (4 bytes, u32)
        //   _reserved_padding            @ 28 (4 bytes, u32)
        // Total = 32, align = 8.
        assert_eq!(size_of::<VulkanComputeKernel>(), 32);
        assert_eq!(align_of::<VulkanComputeKernel>(), 8);
        assert_eq!(offset_of!(VulkanComputeKernel, handle), 0);
        assert_eq!(offset_of!(VulkanComputeKernel, vtable), 8);
        assert_eq!(offset_of!(VulkanComputeKernel, methods_vtable), 16);
        assert_eq!(
            offset_of!(VulkanComputeKernel, cached_push_constant_size),
            24
        );
        assert_eq!(offset_of!(VulkanComputeKernel, _reserved_padding), 28);
    }

    #[test]
    fn vulkan_compute_kernel_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<VulkanComputeKernel>();
    }
}

// ---- Validation + creation helpers --------------------------------------------

fn validate_against_spirv(descriptor: &ComputeKernelDescriptor<'_>) -> Result<()> {
    let reflection = Reflection::new_from_spirv(descriptor.spv).map_err(|e| {
        Error::GpuError(format!(
            "Compute kernel '{}': failed to reflect SPIR-V: {e:?}",
            descriptor.label
        ))
    })?;

    let sets = reflection.get_descriptor_sets().map_err(|e| {
        Error::GpuError(format!(
            "Compute kernel '{}': failed to extract descriptor sets: {e:?}",
            descriptor.label
        ))
    })?;

    // Reject multi-set kernels — out of scope.
    if sets.len() > 1 {
        return Err(Error::GpuError(format!(
            "Compute kernel '{}': only descriptor set 0 is supported; SPIR-V uses sets {:?}",
            descriptor.label,
            sets.keys().collect::<Vec<_>>()
        )));
    }

    let set0 = sets.get(&0);

    // For each declared binding, the SPIR-V must agree on (a) presence and (b) descriptor type.
    for spec in descriptor.bindings {
        let info = set0
            .and_then(|m| m.get(&spec.binding))
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "Compute kernel '{}': binding {} declared but missing in SPIR-V",
                    descriptor.label, spec.binding
                ))
            })?;
        let expected = expected_spirv_type(spec.kind);
        if info.ty != expected {
            return Err(Error::GpuError(format!(
                "Compute kernel '{}': binding {} declared {:?} ({:?}), but SPIR-V has {:?}",
                descriptor.label, spec.binding, spec.kind, expected, info.ty
            )));
        }
    }

    // Conversely, every binding the SPIR-V declares must be present in the
    // declaration — silently leaving a shader binding unset is undefined behavior.
    if let Some(set0) = set0 {
        for (&binding, info) in set0 {
            if !descriptor.bindings.iter().any(|s| s.binding == binding) {
                return Err(Error::GpuError(format!(
                    "Compute kernel '{}': SPIR-V declares binding {} ({:?}, name `{}`) but it is missing from the descriptor",
                    descriptor.label, binding, info.ty, info.name
                )));
            }
        }
    }

    // Push-constant size must match if the shader uses any.
    let push = reflection.get_push_constant_range().map_err(|e| {
        Error::GpuError(format!(
            "Compute kernel '{}': failed to read push-constant range: {e:?}",
            descriptor.label
        ))
    })?;
    match (push, descriptor.push_constant_size) {
        (Some(info), declared) if info.size != declared => {
            return Err(Error::GpuError(format!(
                "Compute kernel '{}': push-constant size mismatch — SPIR-V says {}, descriptor declares {}",
                descriptor.label, info.size, declared
            )));
        }
        (None, declared) if declared > 0 => {
            return Err(Error::GpuError(format!(
                "Compute kernel '{}': descriptor declares {} push-constant bytes but SPIR-V has none",
                descriptor.label, declared
            )));
        }
        _ => {}
    }

    Ok(())
}

fn expected_spirv_type(kind: ComputeBindingKind) -> RDescriptorType {
    match kind {
        ComputeBindingKind::StorageBuffer => RDescriptorType::STORAGE_BUFFER,
        ComputeBindingKind::UniformBuffer => RDescriptorType::UNIFORM_BUFFER,
        ComputeBindingKind::SampledTexture => RDescriptorType::COMBINED_IMAGE_SAMPLER,
        ComputeBindingKind::SampledImage => RDescriptorType::SAMPLED_IMAGE,
        ComputeBindingKind::StorageImage => RDescriptorType::STORAGE_IMAGE,
    }
}

fn descriptor_kind_to_vk(kind: ComputeBindingKind) -> vk::DescriptorType {
    match kind {
        ComputeBindingKind::StorageBuffer => vk::DescriptorType::STORAGE_BUFFER,
        ComputeBindingKind::UniformBuffer => vk::DescriptorType::UNIFORM_BUFFER,
        ComputeBindingKind::SampledTexture => vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
        ComputeBindingKind::SampledImage => vk::DescriptorType::SAMPLED_IMAGE,
        ComputeBindingKind::StorageImage => vk::DescriptorType::STORAGE_IMAGE,
    }
}

fn create_shader_module(
    device: &vulkanalia::Device,
    spirv: &[u32],
    label: &str,
) -> Result<vk::ShaderModule> {
    let info = vk::ShaderModuleCreateInfo::builder().code(spirv).build();
    unsafe { device.create_shader_module(&info, None) }.map_err(|e| {
        Error::GpuError(format!(
            "Compute kernel '{label}': failed to create shader module: {e}"
        ))
    })
}

fn create_descriptor_set_layout(
    device: &vulkanalia::Device,
    bindings: &[ComputeBindingSpec],
    immutable_samplers: &[(u32, vk::Sampler)],
) -> Result<vk::DescriptorSetLayout> {
    // Builder borrows `pImmutableSamplers` from the slice provided to
    // `.immutable_samplers(...)`. Hold each [vk::Sampler; 1] in a stable
    // slot for the duration of the `info.build()` call so the pointers
    // baked into `layout_bindings` are valid through layout creation.
    let immutable_slots: Vec<[vk::Sampler; 1]> = immutable_samplers
        .iter()
        .map(|(_, sampler)| [*sampler])
        .collect();

    let layout_bindings: Vec<vk::DescriptorSetLayoutBinding> = bindings
        .iter()
        .map(|spec| {
            let mut b = vk::DescriptorSetLayoutBinding::builder()
                .binding(spec.binding)
                .descriptor_type(descriptor_kind_to_vk(spec.kind))
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE);
            if let Some(idx) = immutable_samplers
                .iter()
                .position(|(b, _)| *b == spec.binding)
            {
                b = b.immutable_samplers(&immutable_slots[idx]);
            }
            b.build()
        })
        .collect();

    let info = vk::DescriptorSetLayoutCreateInfo::builder()
        .bindings(&layout_bindings)
        .build();
    let layout = unsafe { device.create_descriptor_set_layout(&info, None) }
        .map_err(|e| Error::GpuError(format!("Failed to create descriptor set layout: {e}")))?;
    // Keep `immutable_slots` alive until `create_descriptor_set_layout`
    // has consumed the pointers. (The driver copies the samplers into
    // its internal layout state; the slice can be freed afterward.)
    drop(immutable_slots);
    Ok(layout)
}

fn create_pipeline_layout(
    device: &vulkanalia::Device,
    set_layout: vk::DescriptorSetLayout,
    push_constant_size: u32,
) -> Result<vk::PipelineLayout> {
    let set_layouts = [set_layout];
    let push_constant_ranges: Vec<vk::PushConstantRange> = if push_constant_size > 0 {
        vec![vk::PushConstantRange::builder()
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
            .offset(0)
            .size(push_constant_size)
            .build()]
    } else {
        Vec::new()
    };

    let info = vk::PipelineLayoutCreateInfo::builder()
        .set_layouts(&set_layouts)
        .push_constant_ranges(&push_constant_ranges)
        .build();

    unsafe { device.create_pipeline_layout(&info, None) }
        .map_err(|e| Error::GpuError(format!("Failed to create pipeline layout: {e}")))
}

/// Build a compute pipeline, transparently using an on-disk pipeline cache
/// keyed by the SPIR-V SHA-256.
///
/// The driver validates the cache blob's `VkPipelineCacheHeaderVersionOne`
/// (vendor_id + device_id + cache_uuid) and silently rejects mismatches —
/// fallback-to-recompile is automatic. All cache I/O failures are non-fatal:
/// we warn and fall through to a null cache so kernel construction never
/// fails on a stale, corrupt, or unwritable cache file.
fn create_compute_pipeline_with_cache(
    device: &vulkanalia::Device,
    shader_module: vk::ShaderModule,
    pipeline_layout: vk::PipelineLayout,
    spv: &[u8],
    label: &str,
) -> Result<vk::Pipeline> {
    let cache_path = pipeline_cache_file_path(spv);
    let initial_data = cache_path.as_deref().and_then(read_cache_blob);

    // `Some(cache_handle)` if we successfully created a `VkPipelineCache` —
    // we need to destroy it before returning regardless of success/failure
    // of the pipeline build.
    let pipeline_cache = create_pipeline_cache_handle(device, initial_data.as_deref(), label);

    let cache_handle = pipeline_cache.unwrap_or(vk::PipelineCache::null());

    let stage = vk::PipelineShaderStageCreateInfo::builder()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(shader_module)
        .name(b"main\0")
        .build();
    let info = vk::ComputePipelineCreateInfo::builder()
        .stage(stage)
        .layout(pipeline_layout)
        .build();

    let pipelines_result =
        unsafe { device.create_compute_pipelines(cache_handle, &[info], None) };

    // Persist whatever the driver populated, even if one of the pipelines in
    // a hypothetical multi-pipeline batch failed (we only build one here).
    if pipeline_cache.is_some() {
        if let Some(path) = cache_path.as_deref() {
            persist_pipeline_cache(device, cache_handle, path, label);
        }
        unsafe { device.destroy_pipeline_cache(cache_handle, None) };
    }

    let pipelines = pipelines_result.map_err(|e| {
        Error::GpuError(format!(
            "Compute kernel '{label}': failed to create compute pipeline: {e}"
        ))
    })?;
    Ok(pipelines.0[0])
}

/// Resolve the cache directory.
///
/// Order: `STREAMLIB_PIPELINE_CACHE_DIR` env override → `XDG_CACHE_HOME` (via
/// `dirs::cache_dir()`) joined with `streamlib/pipeline-cache` → `None` if no
/// cache root is resolvable. `None` disables caching for this kernel; the
/// kernel still builds, just without `pInitialData`.
fn pipeline_cache_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(PIPELINE_CACHE_DIR_ENV) {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    dirs::cache_dir().map(|d| d.join("streamlib/pipeline-cache"))
}

/// Compute the cache file path for a given SPIR-V blob, or `None` if no
/// cache directory is resolvable.
///
/// File name is the SHA-256 of the SPIR-V bytes in lowercase hex with a
/// `.bin` suffix. Two SPIR-V blobs that differ by any byte produce
/// distinct file paths; identical blobs hit the same cache file across
/// process restarts.
fn pipeline_cache_file_path(spv: &[u8]) -> Option<PathBuf> {
    let dir = pipeline_cache_dir()?;
    let mut hasher = Sha256::new();
    hasher.update(spv);
    let hash_hex = format!("{:x}", hasher.finalize());
    Some(dir.join(format!("{hash_hex}.bin")))
}

fn read_cache_blob(path: &Path) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) if !bytes.is_empty() => Some(bytes),
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(
                "pipeline cache: unreadable cache file at {}: {e}",
                path.display()
            );
            None
        }
    }
}

fn create_pipeline_cache_handle(
    device: &vulkanalia::Device,
    initial_data: Option<&[u8]>,
    label: &str,
) -> Option<vk::PipelineCache> {
    let mut info = vk::PipelineCacheCreateInfo::builder();
    if let Some(data) = initial_data {
        info = info.initial_data(data);
        tracing::debug!(
            "Compute kernel '{label}': loading pipeline cache (pInitialData {} bytes)",
            data.len()
        );
    } else {
        tracing::debug!(
            "Compute kernel '{label}': pipeline cache cold (no pInitialData)"
        );
    }
    let info = info.build();
    match unsafe { device.create_pipeline_cache(&info, None) } {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::warn!(
                "Compute kernel '{label}': vkCreatePipelineCache failed: {e} — falling back to null cache"
            );
            None
        }
    }
}

fn persist_pipeline_cache(
    device: &vulkanalia::Device,
    cache: vk::PipelineCache,
    path: &Path,
    label: &str,
) {
    let data = match unsafe { device.get_pipeline_cache_data(cache) } {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!(
                "Compute kernel '{label}': vkGetPipelineCacheData failed: {e}"
            );
            return;
        }
    };
    if data.is_empty() {
        // Driver returned no data — nothing to persist.
        return;
    }
    if let Err(e) = atomic_write_pipeline_cache(path, &data) {
        tracing::warn!(
            "Compute kernel '{label}': failed to persist pipeline cache to {}: {e}",
            path.display()
        );
    } else {
        tracing::debug!(
            "Compute kernel '{label}': persisted pipeline cache ({} bytes) to {}",
            data.len(),
            path.display()
        );
    }
}

fn atomic_write_pipeline_cache(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Same-directory temp file → POSIX rename is atomic on the same
    // filesystem. PID + nanos disambiguates concurrent writers; the loser
    // of the race just overwrites the winner, which is fine — both blobs
    // are equally valid and the driver re-validates on next load.
    let suffix = format!(
        "tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let mut tmp = path.to_path_buf();
    tmp.set_extension(format!(
        "bin.{suffix}"
    ));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn create_descriptor_pool(
    device: &vulkanalia::Device,
    bindings: &[ComputeBindingSpec],
) -> Result<vk::DescriptorPool> {
    let mut counts: HashMap<vk::DescriptorType, u32> = HashMap::new();
    for spec in bindings {
        *counts.entry(descriptor_kind_to_vk(spec.kind)).or_insert(0) += 1;
    }
    let pool_sizes: Vec<vk::DescriptorPoolSize> = counts
        .into_iter()
        .map(|(ty, count)| {
            vk::DescriptorPoolSize::builder()
                .type_(ty)
                .descriptor_count(count)
                .build()
        })
        .collect();

    let info = vk::DescriptorPoolCreateInfo::builder()
        .max_sets(1)
        .pool_sizes(&pool_sizes)
        .build();
    unsafe { device.create_descriptor_pool(&info, None) }
        .map_err(|e| Error::GpuError(format!("Failed to create descriptor pool: {e}")))
}

fn allocate_descriptor_set(
    device: &vulkanalia::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
) -> Result<vk::DescriptorSet> {
    let set_layouts = [layout];
    let info = vk::DescriptorSetAllocateInfo::builder()
        .descriptor_pool(pool)
        .set_layouts(&set_layouts)
        .build();
    let sets = unsafe { device.allocate_descriptor_sets(&info) }
        .map_err(|e| Error::GpuError(format!("Failed to allocate descriptor set: {e}")))?;
    Ok(sets[0])
}

fn create_command_pool(
    device: &vulkanalia::Device,
    queue_family_index: u32,
) -> Result<vk::CommandPool> {
    let info = vk::CommandPoolCreateInfo::builder()
        .queue_family_index(queue_family_index)
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
        .build();
    unsafe { device.create_command_pool(&info, None) }
        .map_err(|e| Error::GpuError(format!("Failed to create command pool: {e}")))
}

fn allocate_command_buffer(
    device: &vulkanalia::Device,
    pool: vk::CommandPool,
) -> Result<vk::CommandBuffer> {
    let info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1)
        .build();
    let buffers = unsafe { device.allocate_command_buffers(&info) }
        .map_err(|e| Error::GpuError(format!("Failed to allocate command buffer: {e}")))?;
    Ok(buffers[0])
}

fn vk_image_view_for(texture: &Texture) -> Result<vk::ImageView> {
    use crate::host_rhi::HostTextureExt;
    texture.vulkan_inner().image_view()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::{PixelBuffer, PixelFormat};
    use crate::vulkan::rhi::HostVulkanBuffer;

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(d) => Some(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                None
            }
        }
    }

    /// Allocate a HOST_VISIBLE storage buffer of `element_count * 4` bytes
    /// usable as both an input and output of the test_blend kernel.
    fn make_storage_buffer(
        device: &Arc<HostVulkanDevice>,
        element_count: u32,
    ) -> PixelBuffer {
        let vk_buf = HostVulkanBuffer::new(device, (element_count as u64) * 4)
            .expect("Failed to create storage buffer");
        PixelBuffer::from_host_vulkan_buffer(
            Arc::new(vk_buf),
            element_count,
            1,
            4,
            crate::core::rhi::PixelFormat::Bgra32,
        )
    }

    fn write_buffer_u32(buf: &PixelBuffer, values: &[u32]) {
        let ptr = buf.buffer_ref().inner.mapped_ptr() as *mut u32;
        unsafe {
            std::ptr::copy_nonoverlapping(values.as_ptr(), ptr, values.len());
        }
    }

    fn read_buffer_u32(buf: &PixelBuffer, len: usize) -> Vec<u32> {
        let ptr = buf.buffer_ref().inner.mapped_ptr() as *const u32;
        let mut out = vec![0u32; len];
        unsafe {
            std::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), len);
        }
        out
    }

    fn blend_descriptor(input_count: u32) -> Vec<ComputeBindingSpec> {
        let mut bindings: Vec<ComputeBindingSpec> = (0..input_count)
            .map(ComputeBindingSpec::storage_buffer)
            .collect();
        // Output sits at binding 8 in every variant of test_blend.comp.
        bindings.push(ComputeBindingSpec::storage_buffer(8));
        bindings
    }

    fn blend_spv(input_count: u32) -> &'static [u8] {
        match input_count {
            1 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_1.spv")),
            2 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_2.spv")),
            4 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_4.spv")),
            8 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_8.spv")),
            _ => panic!("test_blend.spv variants are only built for 1/2/4/8"),
        }
    }

    fn run_blend_kernel_for(
        device: &Arc<HostVulkanDevice>,
        input_count: u32,
        element_count: u32,
    ) -> (Vec<PixelBuffer>, PixelBuffer) {
        let bindings = blend_descriptor(input_count);
        let kernel = VulkanComputeKernel::new(
            device,
            &ComputeKernelDescriptor {
                label: "test_blend",
                spv: blend_spv(input_count),
                bindings: &bindings,
                push_constant_size: 4,
            },
        )
        .expect("kernel creation");

        let inputs: Vec<PixelBuffer> = (0..input_count)
            .map(|i| {
                let buf = make_storage_buffer(device, element_count);
                let pattern: Vec<u32> = (0..element_count)
                    .map(|j| (j + 1) * (i + 1))
                    .collect();
                write_buffer_u32(&buf, &pattern);
                buf
            })
            .collect();
        let output = make_storage_buffer(device, element_count);

        for (i, buf) in inputs.iter().enumerate() {
            kernel
                .set_storage_buffer_pixel(i as u32, buf)
                .expect("set_storage_buffer_pixel for input");
        }
        kernel
            .set_storage_buffer_pixel(8, &output)
            .expect("set_storage_buffer_pixel for output");

        let push: [u32; 1] = [element_count];
        kernel.set_push_constants_value(&push).expect("push constants");

        let group_count_x = element_count.div_ceil(64);
        kernel
            .dispatch(group_count_x, 1, 1)
            .expect("kernel dispatch");

        (inputs, output)
    }

    fn expected_blend(inputs: &[PixelBuffer], element_count: u32) -> Vec<u32> {
        let active: Vec<Vec<u32>> = inputs
            .iter()
            .map(|b| read_buffer_u32(b, element_count as usize))
            .collect();
        (0..element_count as usize)
            .map(|j| active.iter().map(|v| v[j]).sum::<u32>())
            .collect()
    }

    fn assert_blend_for(input_count: u32) {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let elem_count = 256u32;
        let (inputs, output) = run_blend_kernel_for(&device, input_count, elem_count);
        let actual = read_buffer_u32(&output, elem_count as usize);
        let expected = expected_blend(&inputs, elem_count);
        assert_eq!(
            actual, expected,
            "blend output mismatch for input_count={input_count}"
        );
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn dispatch_matches_expected_blend_for_one_input() {
        assert_blend_for(1);
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn dispatch_matches_expected_blend_for_two_inputs() {
        assert_blend_for(2);
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn dispatch_matches_expected_blend_for_four_inputs() {
        assert_blend_for(4);
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn dispatch_matches_expected_blend_for_eight_inputs() {
        assert_blend_for(8);
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn kernel_bindings_reflect_descriptor() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        for &input_count in &[1u32, 2, 4, 8] {
            let bindings = blend_descriptor(input_count);
            let kernel = VulkanComputeKernel::new(
                &device,
                &ComputeKernelDescriptor {
                    label: "binding-shape",
                    spv: blend_spv(input_count),
                    bindings: &bindings,
                    push_constant_size: 4,
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

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn rejects_descriptor_with_mismatched_binding_kind() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        // SPIR-V binding 0 is StorageBuffer — declaring it as UniformBuffer must fail.
        let bindings = vec![
            ComputeBindingSpec::uniform_buffer(0),
            ComputeBindingSpec::storage_buffer(8),
        ];
        let result = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                label: "kind-mismatch",
                spv: blend_spv(1),
                bindings: &bindings,
                push_constant_size: 4,
            },
        );
        let err = result.err().expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("binding 0") && msg.contains("UniformBuffer"),
            "expected mismatch error mentioning binding 0 and UniformBuffer, got: {msg}"
        );
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn rejects_descriptor_missing_a_binding_the_shader_declares() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
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
                label: "missing-binding",
                spv: blend_spv(4),
                bindings: &bindings,
                push_constant_size: 4,
            },
        );
        let err = result.err().expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("binding 2"),
            "expected error about missing binding 2, got: {msg}"
        );
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn rejects_descriptor_with_extra_binding_not_in_shader() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        // 1-input shader declares only bindings 0 and 8 — extra binding 1 is invalid.
        let bindings = vec![
            ComputeBindingSpec::storage_buffer(0),
            ComputeBindingSpec::storage_buffer(1),
            ComputeBindingSpec::storage_buffer(8),
        ];
        let result = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                label: "extra-binding",
                spv: blend_spv(1),
                bindings: &bindings,
                push_constant_size: 4,
            },
        );
        let err = result.err().expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("binding 1"),
            "expected error about extra binding 1, got: {msg}"
        );
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn rejects_push_constant_size_mismatch() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let bindings = blend_descriptor(1);
        let result = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                label: "push-size-mismatch",
                spv: blend_spv(1),
                bindings: &bindings,
                push_constant_size: 16, // SPIR-V declares 4 bytes
            },
        );
        let err = result.err().expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("push-constant size mismatch"),
            "expected push-constant size error, got: {msg}"
        );
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn dispatch_without_setting_bindings_fails_loud() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let bindings = blend_descriptor(2);
        let kernel = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                label: "missing-set",
                spv: blend_spv(2),
                bindings: &bindings,
                push_constant_size: 4,
            },
        )
        .expect("kernel creation");
        kernel.set_push_constants_value(&[1u32]).expect("push constants");
        let err = kernel.dispatch(1, 1, 1).err().expect("expected dispatch failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("not set before dispatch"),
            "expected missing-binding error, got: {msg}"
        );
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn dispatch_completes_within_reasonable_budget() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        // Performance smoke: a small kernel build + first dispatch should
        // round-trip in well under a couple seconds on any reasonable GPU.
        // Catches catastrophic regressions like accidentally recreating
        // Vulkan objects per dispatch. The budget is loose enough to absorb
        // queue-mutex contention from sibling kernel tests running in
        // parallel and cold-pipeline-cache compilation on the first run.
        let elem_count = 4096u32;
        let start = std::time::Instant::now();
        let (_inputs, _output) = run_blend_kernel_for(&device, 8, elem_count);
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "kernel build + first dispatch took {elapsed:?} (>1500ms); regression?"
        );
    }

    // ---- Pipeline cache tests ------------------------------------------------
    //
    // Every test that mutates `STREAMLIB_PIPELINE_CACHE_DIR` is marked
    // `#[serial(streamlib_pipeline_cache_env)]` so they serialize against
    // each other regardless of whether they skip the GPU device queue
    // mutex. The `pipeline_cache_dir()` resolution path doesn't need a
    // Vulkan device, so the queue-mutex serialization isn't sufficient
    // by itself. Required for soundness under Rust 2024's `unsafe
    // std::env::set_var` — concurrent reads from sibling test threads
    // would otherwise be UB.

    use serial_test::serial;

    fn unique_cache_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "streamlib-pipeline-cache-{label}-{}-{}",
            std::process::id(),
            nanos
        ))
    }

    /// Run `f` with `STREAMLIB_PIPELINE_CACHE_DIR` set to `dir`, restoring
    /// the previous value (or unsetting) afterward. Callers MUST mark
    /// the test `#[serial(streamlib_pipeline_cache_env)]` — see the
    /// section comment above for why.
    fn with_pipeline_cache_dir<R>(dir: &Path, f: impl FnOnce() -> R) -> R {
        let prev = std::env::var(PIPELINE_CACHE_DIR_ENV).ok();
        // SAFETY: callers serialize via `#[serial(streamlib_pipeline_cache_env)]`,
        // so concurrent env-var reads/writes from sibling tests are not
        // possible.
        unsafe { std::env::set_var(PIPELINE_CACHE_DIR_ENV, dir) };
        let r = f();
        // SAFETY: same as above.
        unsafe {
            match prev {
                Some(v) => std::env::set_var(PIPELINE_CACHE_DIR_ENV, v),
                None => std::env::remove_var(PIPELINE_CACHE_DIR_ENV),
            }
        }
        r
    }

    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn cache_file_path_is_stable_for_same_spirv_and_distinct_for_different() {
        let dir = unique_cache_dir("path-stability");
        with_pipeline_cache_dir(&dir, || {
            let p1 = pipeline_cache_file_path(blend_spv(1)).expect("dir resolves");
            let p2 = pipeline_cache_file_path(blend_spv(1)).expect("dir resolves");
            let p4 = pipeline_cache_file_path(blend_spv(4)).expect("dir resolves");
            assert_eq!(p1, p2, "same SPIR-V must hash to same path");
            assert_ne!(p1, p4, "different SPIR-V must hash to different paths");
            assert!(p1.starts_with(&dir));
            assert!(
                p1.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.ends_with(".bin"))
                    .unwrap_or(false),
                "cache file should end in .bin, got {p1:?}"
            );
        });
    }

    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn env_override_takes_precedence_over_xdg_default() {
        let dir = unique_cache_dir("env-override");
        with_pipeline_cache_dir(&dir, || {
            let resolved = pipeline_cache_dir().expect("env var resolves");
            assert_eq!(resolved, dir);
        });
    }

    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn empty_env_var_falls_through_to_xdg_default() {
        // Whatever the XDG default resolves to is platform-dependent — what we
        // assert is that an explicitly-empty override does NOT short-circuit
        // resolution to an empty path.
        let dir = unique_cache_dir("empty-env");
        let prev = std::env::var(PIPELINE_CACHE_DIR_ENV).ok();
        // SAFETY: serialized via `#[serial(streamlib_pipeline_cache_env)]`
        // above, so concurrent env-var reads/writes from sibling tests
        // are not possible.
        unsafe { std::env::set_var(PIPELINE_CACHE_DIR_ENV, "") };
        let resolved = pipeline_cache_dir();
        unsafe {
            match prev {
                Some(v) => std::env::set_var(PIPELINE_CACHE_DIR_ENV, v),
                None => std::env::remove_var(PIPELINE_CACHE_DIR_ENV),
            }
        }
        // Either dirs::cache_dir() resolves on this host (and we get a
        // non-empty path) or it doesn't (and we get None) — either way is
        // valid; what's invalid is the env var producing the literal "".
        if let Some(p) = resolved {
            assert!(!p.as_os_str().is_empty());
            assert_ne!(p, PathBuf::from(""));
        }
        let _ = dir; // keep the helper's tempdir name in scope
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn cache_miss_writes_cache_file_after_kernel_construction() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let dir = unique_cache_dir("miss-writes");
        with_pipeline_cache_dir(&dir, || {
            assert!(
                !dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none(),
                "tempdir should be empty before kernel construction"
            );
            let bindings = blend_descriptor(1);
            let _kernel = VulkanComputeKernel::new(
                &device,
                &ComputeKernelDescriptor {
                    label: "cache-miss",
                    spv: blend_spv(1),
                    bindings: &bindings,
                    push_constant_size: 4,
                },
            )
            .expect("kernel creation");
            let expected = pipeline_cache_file_path(blend_spv(1)).expect("path");
            assert!(
                expected.exists(),
                "cache file {} should exist after construction",
                expected.display()
            );
            let written = std::fs::read(&expected).expect("read cache");
            assert!(!written.is_empty(), "cache file must not be empty");
            // Header sanity: VkPipelineCacheHeaderVersionOne is 32 bytes,
            // first u32 is header_size = 32, second u32 is header_version = 1.
            let len = u32::from_le_bytes([
                written[0], written[1], written[2], written[3],
            ]);
            let ver = u32::from_le_bytes([
                written[4], written[5], written[6], written[7],
            ]);
            assert!(
                len >= 16 && ver == 1,
                "expected pipeline cache header (len, ver=1), got len={len} ver={ver}"
            );
        });
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn cache_hit_does_not_panic_or_break_kernel_construction() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let dir = unique_cache_dir("hit-reuses");
        with_pipeline_cache_dir(&dir, || {
            let bindings = blend_descriptor(1);
            // First construction populates the cache.
            drop(
                VulkanComputeKernel::new(
                    &device,
                    &ComputeKernelDescriptor {
                        label: "cache-hit/first",
                        spv: blend_spv(1),
                        bindings: &bindings,
                        push_constant_size: 4,
                    },
                )
                .expect("first construction"),
            );
            let cache_path = pipeline_cache_file_path(blend_spv(1)).expect("path");
            let warm_blob = std::fs::read(&cache_path).expect("warm read");
            assert!(!warm_blob.is_empty(), "cache must be populated by first run");

            // Second construction should hit the cache. We can't assert on
            // tracing output deterministically, but we can assert that the
            // file still exists and is non-empty — the warm path read it,
            // built the pipeline, and rewrote (atomic) on success.
            drop(
                VulkanComputeKernel::new(
                    &device,
                    &ComputeKernelDescriptor {
                        label: "cache-hit/second",
                        spv: blend_spv(1),
                        bindings: &bindings,
                        push_constant_size: 4,
                    },
                )
                .expect("second construction"),
            );
            let after = std::fs::read(&cache_path).expect("post-warm read");
            assert!(!after.is_empty());
        });
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn corrupt_cache_blob_falls_back_to_recompile_and_overwrites() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let dir = unique_cache_dir("corrupt-blob");
        with_pipeline_cache_dir(&dir, || {
            std::fs::create_dir_all(&dir).expect("mkdir");
            let cache_path = pipeline_cache_file_path(blend_spv(1)).expect("path");
            // Plant a header-invalid blob that the driver will reject.
            // 32 bytes of zeros has header_version=0 ≠ 1 — driver ignores
            // the data and treats the cache as empty.
            std::fs::write(&cache_path, vec![0u8; 32]).expect("plant corrupt blob");

            let bindings = blend_descriptor(1);
            let kernel = VulkanComputeKernel::new(
                &device,
                &ComputeKernelDescriptor {
                    label: "corrupt-blob",
                    spv: blend_spv(1),
                    bindings: &bindings,
                    push_constant_size: 4,
                },
            )
            .expect("kernel must construct despite corrupt cache");
            drop(kernel);

            let after = std::fs::read(&cache_path).expect("post-recompile read");
            // Driver-rewritten blob has header_version=1 in bytes 4..8.
            let ver = u32::from_le_bytes([after[4], after[5], after[6], after[7]]);
            assert_eq!(
                ver, 1,
                "expected driver to rewrite cache with valid header, got version={ver}"
            );
        });
    }

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    #[serial(streamlib_pipeline_cache_env)]
    fn read_only_cache_dir_does_not_break_kernel_construction() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let dir = unique_cache_dir("readonly-dir");
        with_pipeline_cache_dir(&dir, || {
            std::fs::create_dir_all(&dir).expect("mkdir");
            let mut perms = std::fs::metadata(&dir).unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // r-x for owner only — write attempts fail with EACCES.
                perms.set_mode(0o555);
            }
            std::fs::set_permissions(&dir, perms).expect("set readonly");

            let bindings = blend_descriptor(1);
            let result = VulkanComputeKernel::new(
                &device,
                &ComputeKernelDescriptor {
                    label: "readonly-cache",
                    spv: blend_spv(1),
                    bindings: &bindings,
                    push_constant_size: 4,
                },
            );

            // Restore so the cleanup can rmdir.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut p = std::fs::metadata(&dir).unwrap().permissions();
                p.set_mode(0o755);
                let _ = std::fs::set_permissions(&dir, p);
            }

            assert!(
                result.is_ok(),
                "kernel construction must succeed even when cache_dir is read-only: {result:?}"
            );
        });
    }

    // ---- Sampled-image binding kind tests ------------------------------------

    const SAMPLED_IMAGE_SPV: &[u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/test_sampled_image.spv"));

    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn sampled_image_binding_dispatches_with_raw_view() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };

        let bindings = [
            ComputeBindingSpec::sampled_image(0),
            ComputeBindingSpec::storage_image(1),
        ];
        let kernel = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                label: "test-sampled-image",
                spv: SAMPLED_IMAGE_SPV,
                bindings: &bindings,
                push_constant_size: 0,
            },
        )
        .expect("kernel construction with SampledImage binding");

        // Lock the reflected kind so the SPIR-V → ComputeBindingKind path
        // continues to produce SampledImage. Reverts to SampledTexture
        // would be silently wrong before this test ran.
        assert!(
            kernel
                .bindings()
                .iter()
                .any(|s| s.binding == 0 && s.kind == ComputeBindingKind::SampledImage),
            "expected binding 0 reflected as SampledImage, got {:?}",
            kernel.bindings()
        );

        // Build a tiny 1x1 RGBA8 SAMPLED image (the input) and a 1x1
        // RGBA8 STORAGE image (the output). Each gets its own VkImageView.
        let device_handle = device.device().clone();
        let allocator = device.allocator().clone();
        let make_image = |usage: vk::ImageUsageFlags|
            -> (vk::Image, vulkanalia_vma::Allocation, vk::ImageView) {
                let info = vk::ImageCreateInfo::builder()
                    .image_type(vk::ImageType::_2D)
                    .format(vk::Format::R8G8B8A8_UNORM)
                    .extent(vk::Extent3D { width: 1, height: 1, depth: 1 })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED)
                    .build();
                let alloc_opts = vulkanalia_vma::AllocationOptions {
                    required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
                    ..Default::default()
                };
                use vulkanalia_vma::Alloc;
                let (image, alloc) = unsafe {
                    allocator.create_image(info, &alloc_opts)
                }
                .expect("create_image");
                let view_info = vk::ImageViewCreateInfo::builder()
                    .image(image)
                    .view_type(vk::ImageViewType::_2D)
                    .format(vk::Format::R8G8B8A8_UNORM)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .build();
                let view = unsafe {
                    device_handle.create_image_view(&view_info, None)
                }
                .expect("create_image_view");
                (image, alloc, view)
            };

        let (in_image, in_alloc, in_view) =
            make_image(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST);
        let (out_image, out_alloc, out_view) =
            make_image(vk::ImageUsageFlags::STORAGE);

        // Transition both images out of UNDEFINED so the shader can sample
        // / write without producing undefined contents on debug drivers.
        // One-shot command buffer.
        use vulkanalia::vk::HasBuilder;
        let queue_family_index = device.queue_family_index();
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT)
            .build();
        let pool = unsafe { device_handle.create_command_pool(&pool_info, None) }
            .expect("create_command_pool");
        let alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1)
            .build();
        let cb = unsafe { device_handle.allocate_command_buffers(&alloc_info) }
            .expect("allocate_command_buffers")[0];
        let begin = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();
        unsafe { device_handle.begin_command_buffer(cb, &begin) }
            .expect("begin_command_buffer");

        let barriers = [
            vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::NONE)
                .src_access_mask(vk::AccessFlags2::empty())
                .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(in_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .build(),
            vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::NONE)
                .src_access_mask(vk::AccessFlags2::empty())
                .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(out_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .build(),
        ];
        let dep = vk::DependencyInfo::builder().image_memory_barriers(&barriers).build();
        unsafe { device_handle.cmd_pipeline_barrier2(cb, &dep) };
        unsafe { device_handle.end_command_buffer(cb) }
            .expect("end_command_buffer");

        let fence_info = vk::FenceCreateInfo::builder().build();
        let fence = unsafe { device_handle.create_fence(&fence_info, None) }
            .expect("create_fence");
        let cb_submit = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cb)
            .build();
        let cb_submits = [cb_submit];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cb_submits)
            .build();
        unsafe {
            device
                .submit_to_queue(device.queue(), &[submit], fence)
                .expect("submit_to_queue");
        }
        unsafe {
            device_handle
                .wait_for_fences(&[fence], true, u64::MAX)
                .expect("wait_for_fences");
            device_handle.destroy_fence(fence, None);
            device_handle.destroy_command_pool(pool, None);
        }

        // Bind via the raw-view setters and dispatch. The shader is a
        // single-thread no-op-ish texelFetch — what we lock is that
        // descriptor-layout + descriptor write + bind + dispatch run
        // end-to-end without validation errors.
        kernel
            .set_sampled_image_view(0, in_view)
            .expect("set_sampled_image_view");
        kernel
            .set_storage_image_view(1, out_view)
            .expect("set_storage_image_view");
        kernel.dispatch(1, 1, 1).expect("dispatch");

        // Cleanup the test resources we allocated outside the kernel.
        unsafe {
            device_handle.destroy_image_view(in_view, None);
            device_handle.destroy_image_view(out_view, None);
            use vulkanalia_vma::Alloc;
            allocator.destroy_image(in_image, in_alloc);
            allocator.destroy_image(out_image, out_alloc);
        }
    }

}

