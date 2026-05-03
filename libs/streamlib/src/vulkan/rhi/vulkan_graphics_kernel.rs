// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan graphics-kernel RHI: multi-stage shader pipeline + descriptor-set
//! ring + per-frame draw primitives.
//!
//! Mirrors [`super::vulkan_compute_kernel::VulkanComputeKernel`] for graphics
//! pipelines: declare the stages and binding shape once, the kernel reflects
//! every stage's SPIR-V on construction, validates the merged declaration
//! against the shaders, and exposes typed setters by slot.
//!
//! Render-loop shape:
//!
//! ```text
//!     // per frame, caller indexes into a ring of `descriptor_sets_in_flight`:
//!     let frame = current_frame % descriptor_sets_in_flight;
//!     kernel.set_sampled_texture(frame, 0, &texture)?;
//!     kernel.set_push_constants_value(frame, &push)?;
//!
//!     device.cmd_begin_rendering(cmd, &rendering_info);  // caller-managed
//!     kernel.cmd_bind_and_draw(cmd, frame, &DrawCall { ... })?;
//!     device.cmd_end_rendering(cmd);                      // caller-managed
//! ```
//!
//! The caller drives render-pass scope (begin/end-rendering) because the
//! same render pass typically dispatches multiple kernels. The kernel
//! records `vkCmdBindPipeline` / `vkCmdSetViewport` / `vkCmdSetScissor` /
//! `vkCmdBindDescriptorSets` / `vkCmdPushConstants` /
//! `vkCmdBindVertexBuffers` / `vkCmdBindIndexBuffer` /
//! `vkCmdDraw[Indexed]` and lets the caller manage everything else
//! (command buffer submission, layout transitions on inputs, fence /
//! semaphore signaling, presentation).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use rspirv_reflect::{DescriptorType as RDescriptorType, Reflection};

use crate::core::rhi::{
    BlendFactor, BlendOp, ColorBlendState, ColorWriteMask, CullMode, DepthCompareOp, DepthFormat,
    DepthStencilState, DrawCall, DrawIndexedCall, FrontFace, GraphicsBindingKind,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState,
    GraphicsShaderStage, GraphicsShaderStageFlags, GraphicsStage, IndexType, PolygonMode,
    PrimitiveTopology, RhiPixelBuffer, ScissorRect, StreamTexture, TextureFormat,
    VertexAttributeFormat, VertexInputRate, VertexInputState, Viewport,
};
use crate::core::{Result, StreamError};

use super::HostVulkanDevice;

/// Env var that overrides the default pipeline-cache directory. Shared with
/// [`super::vulkan_compute_kernel`] so cached pipelines for both kernel
/// kinds live under the same root.
pub const PIPELINE_CACHE_DIR_ENV: &str = "STREAMLIB_PIPELINE_CACHE_DIR";

/// One graphics kernel: multi-stage pipeline + descriptor-set ring + per-frame
/// draw primitives.
pub struct VulkanGraphicsKernel {
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
    /// Per-slot pending state for staged setters; `cmd_bind_and_draw*` flushes
    /// for the slot it's targeting.
    pending: Mutex<Vec<PendingState>>,
    /// Lazy default sampler for [`GraphicsBindingKind::SampledTexture`] bindings.
    default_sampler: Mutex<Option<vk::Sampler>>,
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
    pub texture: &'a StreamTexture,
    /// `Some` clears the attachment to this RGBA value before drawing;
    /// `None` loads existing contents.
    pub clear_color: Option<[f32; 4]>,
}

/// Draw variant for [`VulkanGraphicsKernel::offscreen_render`].
pub enum OffscreenDraw {
    Draw(DrawCall),
    DrawIndexed(DrawIndexedCall),
}

impl VulkanGraphicsKernel {
    /// Create a new graphics kernel from a multi-stage SPIR-V set + binding
    /// declaration + pipeline state.
    ///
    /// Reflects every stage's SPIR-V via `rspirv-reflect`, validates that the
    /// declared `bindings` match the merged shader declaration, and rejects
    /// any mismatch before allocating Vulkan objects.
    pub fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &GraphicsKernelDescriptor<'_>,
    ) -> Result<Self> {
        if descriptor.descriptor_sets_in_flight == 0 {
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{}': descriptor_sets_in_flight must be ≥ 1",
                descriptor.label
            )));
        }
        if descriptor.stages.is_empty() {
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{}': no shader stages provided",
                descriptor.label
            )));
        }
        // Today only Vertex + Fragment are supported; reject any other stage
        // up front so the descriptor stays open-shape.
        for stage in descriptor.stages {
            match stage.stage {
                GraphicsShaderStage::Vertex | GraphicsShaderStage::Fragment => {}
            }
        }
        // Require exactly one Vertex stage.
        let vertex_count = descriptor
            .stages
            .iter()
            .filter(|s| s.stage == GraphicsShaderStage::Vertex)
            .count();
        if vertex_count != 1 {
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{}': expected exactly 1 Vertex stage, got {vertex_count}",
                descriptor.label
            )));
        }
        // Fragment is optional in Vulkan but every consumer in tree uses one;
        // require it for the v1 surface to keep the contract simple.
        let fragment_count = descriptor
            .stages
            .iter()
            .filter(|s| s.stage == GraphicsShaderStage::Fragment)
            .count();
        if fragment_count != 1 {
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{}': expected exactly 1 Fragment stage, got {fragment_count}",
                descriptor.label
            )));
        }

        validate_against_spirv(descriptor)?;

        let device = vulkan_device.device();
        let queue = vulkan_device.queue();
        let queue_family_index = vulkan_device.queue_family_index();

        // ---- Vulkan objects -----------------------------------------------------
        // Strict creation order with staged cleanup on each error edge so we
        // never leak partial state.
        let shader_modules = create_shader_modules(device, descriptor)?;

        let descriptor_set_layout = match create_descriptor_set_layout(
            device,
            descriptor.bindings,
        ) {
            Ok(l) => l,
            Err(e) => {
                destroy_shader_modules(device, &shader_modules);
                return Err(e);
            }
        };

        let pipeline_layout = match create_pipeline_layout(
            device,
            descriptor_set_layout,
            descriptor.push_constants,
        ) {
            Ok(l) => l,
            Err(e) => {
                unsafe { device.destroy_descriptor_set_layout(descriptor_set_layout, None) };
                destroy_shader_modules(device, &shader_modules);
                return Err(e);
            }
        };

        let pipeline = match create_graphics_pipeline_with_cache(
            device,
            &shader_modules,
            descriptor.stages,
            pipeline_layout,
            &descriptor.pipeline_state,
            descriptor.label,
        ) {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                destroy_shader_modules(device, &shader_modules);
                return Err(e);
            }
        };

        // Shader modules are no longer needed after pipeline creation.
        destroy_shader_modules(device, &shader_modules);

        let descriptor_pool = match create_descriptor_pool(
            device,
            descriptor.bindings,
            descriptor.descriptor_sets_in_flight,
        ) {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err(e);
            }
        };

        let descriptor_sets = match allocate_descriptor_sets(
            device,
            descriptor_pool,
            descriptor_set_layout,
            descriptor.descriptor_sets_in_flight,
        ) {
            Ok(s) => s,
            Err(e) => {
                unsafe {
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err(e);
            }
        };

        let pending: Vec<PendingState> = (0..descriptor.descriptor_sets_in_flight)
            .map(|_| PendingState::new())
            .collect();

        Ok(Self {
            label: descriptor.label.to_string(),
            vulkan_device: Arc::clone(vulkan_device),
            device: device.clone(),
            queue,
            queue_family_index,
            bindings: descriptor.bindings.to_vec(),
            push_constant_size: descriptor.push_constants.size,
            push_constant_stages: shader_stage_flags_to_vk(descriptor.push_constants.stages),
            pipeline_state: descriptor.pipeline_state.clone(),
            descriptor_sets_in_flight: descriptor.descriptor_sets_in_flight,
            pipeline,
            pipeline_layout,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_sets,
            pending: Mutex::new(pending),
            default_sampler: Mutex::new(None),
            offscreen: Mutex::new(None),
        })
    }

    /// Bindings declared at construction time.
    pub fn bindings(&self) -> &[GraphicsBindingSpec] {
        &self.bindings
    }

    /// Push-constant size in bytes (0 if the kernel has none).
    pub fn push_constant_size(&self) -> u32 {
        self.push_constant_size
    }

    /// Number of descriptor sets in the ring.
    pub fn descriptor_sets_in_flight(&self) -> u32 {
        self.descriptor_sets_in_flight
    }

    /// Bind a sampled texture at `(frame_index, binding)`. Uses the kernel's
    /// default linear-clamp sampler.
    pub fn set_sampled_texture(
        &self,
        frame_index: u32,
        binding: u32,
        texture: &StreamTexture,
    ) -> Result<()> {
        self.expect_kind(binding, GraphicsBindingKind::SampledTexture)?;
        let view = texture.inner.image_view()?;
        let sampler = self.default_sampler()?;
        self.with_slot(frame_index, |slot| {
            slot.bindings.insert(
                binding,
                BindingResource::SampledImage { view, sampler },
            );
            Ok(())
        })
    }

    /// Bind a storage buffer at `(frame_index, binding)`.
    pub fn set_storage_buffer(
        &self,
        frame_index: u32,
        binding: u32,
        buffer: &RhiPixelBuffer,
    ) -> Result<()> {
        self.expect_kind(binding, GraphicsBindingKind::StorageBuffer)?;
        let (vk_buf, size) = vk_buffer_for(buffer);
        self.with_slot(frame_index, |slot| {
            slot.bindings.insert(
                binding,
                BindingResource::Buffer { buffer: vk_buf, size },
            );
            Ok(())
        })
    }

    /// Bind a uniform buffer at `(frame_index, binding)`.
    pub fn set_uniform_buffer(
        &self,
        frame_index: u32,
        binding: u32,
        buffer: &RhiPixelBuffer,
    ) -> Result<()> {
        self.expect_kind(binding, GraphicsBindingKind::UniformBuffer)?;
        let (vk_buf, size) = vk_buffer_for(buffer);
        self.with_slot(frame_index, |slot| {
            slot.bindings.insert(
                binding,
                BindingResource::Buffer { buffer: vk_buf, size },
            );
            Ok(())
        })
    }

    /// Bind a storage image at `(frame_index, binding)`.
    pub fn set_storage_image(
        &self,
        frame_index: u32,
        binding: u32,
        texture: &StreamTexture,
    ) -> Result<()> {
        self.expect_kind(binding, GraphicsBindingKind::StorageImage)?;
        let view = texture.inner.image_view()?;
        self.with_slot(frame_index, |slot| {
            slot.bindings
                .insert(binding, BindingResource::StorageImage { view });
            Ok(())
        })
    }

    /// Stage push constants for `frame_index`. Size must match the kernel's
    /// declared `push_constants.size`.
    pub fn set_push_constants(&self, frame_index: u32, bytes: &[u8]) -> Result<()> {
        if bytes.len() as u32 != self.push_constant_size {
            return Err(StreamError::GpuError(format!(
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
    pub fn set_push_constants_value<T: Copy>(
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
    pub fn set_vertex_buffer(
        &self,
        frame_index: u32,
        binding: u32,
        buffer: &RhiPixelBuffer,
        offset: u64,
    ) -> Result<()> {
        match &self.pipeline_state.vertex_input {
            VertexInputState::None => {
                return Err(StreamError::GpuError(format!(
                    "Graphics kernel '{}': set_vertex_buffer called but pipeline has no vertex input bindings",
                    self.label
                )));
            }
            VertexInputState::Buffers { bindings, .. } => {
                if !bindings.iter().any(|b| b.binding == binding) {
                    return Err(StreamError::GpuError(format!(
                        "Graphics kernel '{}': vertex binding {binding} not declared in pipeline",
                        self.label
                    )));
                }
            }
        }
        let (vk_buf, _) = vk_buffer_for(buffer);
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
    pub fn set_index_buffer(
        &self,
        frame_index: u32,
        buffer: &RhiPixelBuffer,
        offset: u64,
        index_type: IndexType,
    ) -> Result<()> {
        let (vk_buf, _) = vk_buffer_for(buffer);
        self.with_slot(frame_index, |slot| {
            slot.index_buffer = Some(BoundIndexBuffer {
                buffer: vk_buf,
                offset,
                index_type: index_type_to_vk(index_type),
            });
            Ok(())
        })
    }

    /// Record bind + push + draw into `command_buffer` for the given
    /// `frame_index`. Caller is responsible for the surrounding render-pass
    /// scope (`vkCmdBeginRendering` / `vkCmdEndRendering`).
    pub fn cmd_bind_and_draw(
        &self,
        command_buffer: vk::CommandBuffer,
        frame_index: u32,
        draw: &DrawCall,
    ) -> Result<()> {
        self.cmd_bind_and_draw_inner(command_buffer, frame_index, DrawKind::Draw(*draw))
    }

    /// Indexed variant of [`Self::cmd_bind_and_draw`]. Caller must have set
    /// an index buffer for `frame_index` via [`Self::set_index_buffer`].
    pub fn cmd_bind_and_draw_indexed(
        &self,
        command_buffer: vk::CommandBuffer,
        frame_index: u32,
        draw: &DrawIndexedCall,
    ) -> Result<()> {
        self.cmd_bind_and_draw_inner(
            command_buffer,
            frame_index,
            DrawKind::DrawIndexed(*draw),
        )
    }

    /// Render into one or more offscreen color attachments using slot 0 of
    /// the descriptor ring. Owns its own command buffer and fence; submits,
    /// waits, and returns. Convenience for unit tests and one-shot
    /// renderers.
    pub fn offscreen_render(
        &self,
        frame_index: u32,
        color_targets: &[OffscreenColorTarget<'_>],
        extent: (u32, u32),
        draw: OffscreenDraw,
    ) -> Result<()> {
        if color_targets.is_empty() {
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{}': offscreen_render called with no color targets",
                self.label
            )));
        }
        let scaffold_handles = self.offscreen_scaffold()?;
        let (command_buffer, fence) = (
            scaffold_handles.command_buffer,
            scaffold_handles.fence,
        );

        let (width, height) = extent;
        let viewport = Viewport::full(width, height);
        let scissor = ScissorRect::full(width, height);

        unsafe {
            self.device
                .wait_for_fences(&[fence], true, u64::MAX)
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "Graphics kernel '{}': wait_for_fences failed: {e}",
                        self.label
                    ))
                })?;
            self.device.reset_fences(&[fence]).map_err(|e| {
                StreamError::GpuError(format!(
                    "Graphics kernel '{}': reset_fences failed: {e}",
                    self.label
                ))
            })?;

            self.device
                .reset_command_buffer(command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "Graphics kernel '{}': reset_command_buffer failed: {e}",
                        self.label
                    ))
                })?;

            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build();
            self.device
                .begin_command_buffer(command_buffer, &begin_info)
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "Graphics kernel '{}': begin_command_buffer failed: {e}",
                        self.label
                    ))
                })?;

            // Transition every color target UNDEFINED → COLOR_ATTACHMENT_OPTIMAL.
            // Caller-supplied targets are expected to be freshly-acquired or
            // post-presented; UNDEFINED with CLEAR/LOAD load_op is the
            // tolerant pattern.
            let mut barriers: Vec<vk::ImageMemoryBarrier2> = Vec::with_capacity(color_targets.len());
            for target in color_targets {
                let image = target.texture.inner.image().ok_or_else(|| {
                    StreamError::GpuError(format!(
                        "Graphics kernel '{}': offscreen color target has no VkImage",
                        self.label
                    ))
                })?;
                barriers.push(
                    vk::ImageMemoryBarrier2::builder()
                        .src_stage_mask(vk::PipelineStageFlags2::NONE)
                        .src_access_mask(vk::AccessFlags2::NONE)
                        .dst_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
                        .dst_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
                        .old_layout(vk::ImageLayout::UNDEFINED)
                        .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(image)
                        .subresource_range(
                            vk::ImageSubresourceRange::builder()
                                .aspect_mask(vk::ImageAspectFlags::COLOR)
                                .base_mip_level(0)
                                .level_count(1)
                                .base_array_layer(0)
                                .layer_count(1)
                                .build(),
                        )
                        .build(),
                );
            }
            let dep = vk::DependencyInfo::builder()
                .image_memory_barriers(&barriers)
                .build();
            self.device.cmd_pipeline_barrier2(command_buffer, &dep);

            // Build dynamic-rendering color attachments.
            let color_attachments: Vec<vk::RenderingAttachmentInfo> = color_targets
                .iter()
                .map(|t| {
                    let view = t.texture.inner.image_view().unwrap_or(vk::ImageView::null());
                    let mut attachment = vk::RenderingAttachmentInfo::builder()
                        .image_view(view)
                        .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                        .store_op(vk::AttachmentStoreOp::STORE);
                    attachment = if let Some(c) = t.clear_color {
                        attachment
                            .load_op(vk::AttachmentLoadOp::CLEAR)
                            .clear_value(vk::ClearValue {
                                color: vk::ClearColorValue { float32: c },
                            })
                    } else {
                        attachment.load_op(vk::AttachmentLoadOp::LOAD)
                    };
                    attachment.build()
                })
                .collect();
            let rendering_info = vk::RenderingInfo::builder()
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: vk::Extent2D { width, height },
                })
                .layer_count(1)
                .color_attachments(&color_attachments)
                .build();
            self.device.cmd_begin_rendering(command_buffer, &rendering_info);

            // Drop the immutable borrow on `draw` here so we can build a
            // DrawCall/DrawIndexedCall with the offscreen viewport/scissor
            // (only matters when the kernel has dynamic viewport/scissor).
        }

        let offscreen_draw = match draw {
            OffscreenDraw::Draw(d) => DrawKind::Draw(DrawCall {
                viewport: d.viewport.or(Some(viewport)),
                scissor: d.scissor.or(Some(scissor)),
                ..d
            }),
            OffscreenDraw::DrawIndexed(d) => DrawKind::DrawIndexed(DrawIndexedCall {
                viewport: d.viewport.or(Some(viewport)),
                scissor: d.scissor.or(Some(scissor)),
                ..d
            }),
        };
        self.cmd_bind_and_draw_inner(command_buffer, frame_index, offscreen_draw)?;

        unsafe {
            self.device.cmd_end_rendering(command_buffer);

            self.device
                .end_command_buffer(command_buffer)
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "Graphics kernel '{}': end_command_buffer failed: {e}",
                        self.label
                    ))
                })?;

            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(command_buffer)
                .build();
            let cmd_infos = [cmd_info];
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .build();
            self.vulkan_device
                .submit_to_queue(self.queue, &[submit], fence)?;

            self.device
                .wait_for_fences(&[fence], true, u64::MAX)
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "Graphics kernel '{}': post-submit wait failed: {e}",
                        self.label
                    ))
                })?;
        }
        Ok(())
    }

    fn cmd_bind_and_draw_inner(
        &self,
        command_buffer: vk::CommandBuffer,
        frame_index: u32,
        draw: DrawKind,
    ) -> Result<()> {
        if frame_index >= self.descriptor_sets_in_flight {
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{}': frame_index {frame_index} out of range (ring depth {})",
                self.label, self.descriptor_sets_in_flight
            )));
        }
        // Drain pending state for this slot up-front to avoid leaks across
        // back-to-back records.
        let pending = {
            let mut guard = self.pending.lock();
            let slot = &mut guard[frame_index as usize];
            PendingState {
                bindings: std::mem::take(&mut slot.bindings),
                push_constants: std::mem::take(&mut slot.push_constants),
                vertex_buffers: std::mem::take(&mut slot.vertex_buffers),
                index_buffer: slot.index_buffer.take(),
            }
        };

        for spec in &self.bindings {
            if !pending.bindings.contains_key(&spec.binding) {
                return Err(StreamError::GpuError(format!(
                    "Graphics kernel '{}': binding {} ({:?}) not set before draw",
                    self.label, spec.binding, spec.kind
                )));
            }
        }
        if self.push_constant_size > 0 && pending.push_constants.is_empty() {
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{}': push constants not set before draw",
                self.label
            )));
        }
        if let VertexInputState::Buffers { bindings, .. } = &self.pipeline_state.vertex_input {
            for vb in bindings {
                if !pending.vertex_buffers.contains_key(&vb.binding) {
                    return Err(StreamError::GpuError(format!(
                        "Graphics kernel '{}': vertex buffer at binding {} not set before draw",
                        self.label, vb.binding
                    )));
                }
            }
        }
        if matches!(draw, DrawKind::DrawIndexed(_)) && pending.index_buffer.is_none() {
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{}': indexed draw requested but no index buffer set",
                self.label
            )));
        }

        self.flush_descriptor_writes(frame_index, &pending)?;

        let descriptor_set = self.descriptor_sets[frame_index as usize];

        unsafe {
            self.device.cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline,
            );

            if self.pipeline_state.dynamic_state == GraphicsDynamicState::ViewportScissor {
                let (viewport_opt, scissor_opt) = match &draw {
                    DrawKind::Draw(d) => (d.viewport, d.scissor),
                    DrawKind::DrawIndexed(d) => (d.viewport, d.scissor),
                };
                let viewport = viewport_opt.ok_or_else(|| {
                    StreamError::GpuError(format!(
                        "Graphics kernel '{}': pipeline has dynamic viewport but DrawCall has none",
                        self.label
                    ))
                })?;
                let scissor = scissor_opt.ok_or_else(|| {
                    StreamError::GpuError(format!(
                        "Graphics kernel '{}': pipeline has dynamic scissor but DrawCall has none",
                        self.label
                    ))
                })?;
                self.device.cmd_set_viewport(
                    command_buffer,
                    0,
                    &[vk::Viewport {
                        x: viewport.x,
                        y: viewport.y,
                        width: viewport.width,
                        height: viewport.height,
                        min_depth: viewport.min_depth,
                        max_depth: viewport.max_depth,
                    }],
                );
                self.device.cmd_set_scissor(
                    command_buffer,
                    0,
                    &[vk::Rect2D {
                        offset: vk::Offset2D {
                            x: scissor.x,
                            y: scissor.y,
                        },
                        extent: vk::Extent2D {
                            width: scissor.width,
                            height: scissor.height,
                        },
                    }],
                );
            }

            self.device.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                &[descriptor_set],
                &[],
            );

            if self.push_constant_size > 0 {
                self.device.cmd_push_constants(
                    command_buffer,
                    self.pipeline_layout,
                    self.push_constant_stages,
                    0,
                    &pending.push_constants,
                );
            }

            // Vertex buffers — bind in declared order.
            if let VertexInputState::Buffers { bindings, .. } =
                &self.pipeline_state.vertex_input
            {
                let mut sorted: Vec<u32> = bindings.iter().map(|b| b.binding).collect();
                sorted.sort_unstable();
                if let (Some(&first), Some(&last)) = (sorted.first(), sorted.last()) {
                    let count = (last - first + 1) as usize;
                    if sorted == (first..=last).collect::<Vec<_>>() {
                        // Contiguous range — single bind call.
                        let buffers: Vec<vk::Buffer> = sorted
                            .iter()
                            .map(|b| pending.vertex_buffers.get(b).expect("checked").buffer)
                            .collect();
                        let offsets: Vec<vk::DeviceSize> = sorted
                            .iter()
                            .map(|b| pending.vertex_buffers.get(b).expect("checked").offset)
                            .collect();
                        self.device.cmd_bind_vertex_buffers(
                            command_buffer,
                            first,
                            &buffers,
                            &offsets,
                        );
                        let _ = count;
                    } else {
                        // Non-contiguous — bind each individually.
                        for &b in &sorted {
                            let bb = pending.vertex_buffers.get(&b).expect("checked");
                            self.device.cmd_bind_vertex_buffers(
                                command_buffer,
                                b,
                                &[bb.buffer],
                                &[bb.offset],
                            );
                        }
                    }
                }
            }

            // Index buffer — only bound when an indexed draw is recorded.
            if let DrawKind::DrawIndexed(_) = &draw {
                let ib = pending.index_buffer.expect("checked above");
                self.device
                    .cmd_bind_index_buffer(command_buffer, ib.buffer, ib.offset, ib.index_type);
            }

            match draw {
                DrawKind::Draw(d) => {
                    self.device.cmd_draw(
                        command_buffer,
                        d.vertex_count,
                        d.instance_count,
                        d.first_vertex,
                        d.first_instance,
                    );
                }
                DrawKind::DrawIndexed(d) => {
                    self.device.cmd_draw_indexed(
                        command_buffer,
                        d.index_count,
                        d.instance_count,
                        d.first_index,
                        d.vertex_offset,
                        d.first_instance,
                    );
                }
            }
        }
        Ok(())
    }

    fn expect_kind(&self, binding: u32, expected: GraphicsBindingKind) -> Result<()> {
        let spec = self
            .bindings
            .iter()
            .find(|b| b.binding == binding)
            .ok_or_else(|| {
                StreamError::GpuError(format!(
                    "Graphics kernel '{}': binding {binding} not declared",
                    self.label
                ))
            })?;
        if spec.kind != expected {
            return Err(StreamError::GpuError(format!(
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
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{}': frame_index {frame_index} out of range (ring depth {})",
                self.label, self.descriptor_sets_in_flight
            )));
        }
        let mut guard = self.pending.lock();
        f(&mut guard[frame_index as usize])
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
            .map_err(|e| StreamError::GpuError(format!("Failed to create default sampler: {e}")))?;
        *guard = Some(sampler);
        Ok(sampler)
    }

    fn offscreen_scaffold(&self) -> Result<OffscreenScaffoldHandles> {
        let mut guard = self.offscreen.lock();
        if let Some(s) = guard.as_ref() {
            return Ok(OffscreenScaffoldHandles {
                command_buffer: s.command_buffer,
                fence: s.fence,
            });
        }
        let command_pool = create_command_pool(&self.device, self.queue_family_index)?;
        let command_buffer = match allocate_command_buffer(&self.device, command_pool) {
            Ok(b) => b,
            Err(e) => {
                unsafe { self.device.destroy_command_pool(command_pool, None) };
                return Err(e);
            }
        };
        let fence_info = vk::FenceCreateInfo::builder()
            .flags(vk::FenceCreateFlags::SIGNALED)
            .build();
        let fence = match unsafe { self.device.create_fence(&fence_info, None) } {
            Ok(f) => f,
            Err(e) => {
                unsafe { self.device.destroy_command_pool(command_pool, None) };
                return Err(StreamError::GpuError(format!(
                    "Failed to create offscreen fence: {e}"
                )));
            }
        };
        *guard = Some(OffscreenScaffold {
            command_pool,
            command_buffer,
            fence,
        });
        Ok(OffscreenScaffoldHandles {
            command_buffer,
            fence,
        })
    }

    fn flush_descriptor_writes(
        &self,
        frame_index: u32,
        pending: &PendingState,
    ) -> Result<()> {
        let descriptor_set = self.descriptor_sets[frame_index as usize];

        let mut buffer_infos: Vec<vk::DescriptorBufferInfo> =
            Vec::with_capacity(self.bindings.len());
        let mut image_infos: Vec<vk::DescriptorImageInfo> = Vec::with_capacity(self.bindings.len());

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
                (GraphicsBindingKind::StorageBuffer, BindingResource::Buffer { buffer, size }) => {
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
                (GraphicsBindingKind::UniformBuffer, BindingResource::Buffer { buffer, size }) => {
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
                    GraphicsBindingKind::SampledTexture,
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
                (GraphicsBindingKind::StorageImage, BindingResource::StorageImage { view }) => {
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
                    return Err(StreamError::GpuError(format!(
                        "Graphics kernel '{}': binding {} kind/resource mismatch (declared {:?})",
                        self.label, spec.binding, spec.kind
                    )));
                }
            }
        }

        let mut writes: Vec<vk::WriteDescriptorSet> = Vec::with_capacity(slots.len());
        for slot in &slots {
            let mut write = vk::WriteDescriptorSet::builder()
                .dst_set(descriptor_set)
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

#[derive(Clone, Copy)]
struct OffscreenScaffoldHandles {
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
}

#[derive(Clone, Copy)]
enum DrawKind {
    Draw(DrawCall),
    DrawIndexed(DrawIndexedCall),
}

impl Drop for VulkanGraphicsKernel {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            if let Some(scaffold) = self.offscreen.lock().take() {
                self.device.destroy_fence(scaffold.fence, None);
                self.device.destroy_command_pool(scaffold.command_pool, None);
            }
            if let Some(sampler) = self.default_sampler.lock().take() {
                self.device.destroy_sampler(sampler, None);
            }
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
unsafe impl Send for VulkanGraphicsKernel {}
unsafe impl Sync for VulkanGraphicsKernel {}

impl std::fmt::Debug for VulkanGraphicsKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanGraphicsKernel")
            .field("label", &self.label)
            .field("bindings", &self.bindings)
            .field("push_constant_size", &self.push_constant_size)
            .field("descriptor_sets_in_flight", &self.descriptor_sets_in_flight)
            .finish()
    }
}

// ---- Validation + creation helpers --------------------------------------------

fn validate_against_spirv(descriptor: &GraphicsKernelDescriptor<'_>) -> Result<()> {
    use std::collections::BTreeMap;

    let mut merged: BTreeMap<u32, (RDescriptorType, GraphicsShaderStageFlags)> = BTreeMap::new();
    let mut spirv_push_size: u32 = 0;
    let mut spirv_push_stages = GraphicsShaderStageFlags::NONE;

    for stage in descriptor.stages {
        let stage_flag = stage_to_flag(stage.stage);
        let reflection = Reflection::new_from_spirv(stage.spv).map_err(|e| {
            StreamError::GpuError(format!(
                "Graphics kernel '{}': failed to reflect SPIR-V for {:?} stage: {e:?}",
                descriptor.label, stage.stage
            ))
        })?;
        let sets = reflection.get_descriptor_sets().map_err(|e| {
            StreamError::GpuError(format!(
                "Graphics kernel '{}': failed to extract descriptor sets for {:?} stage: {e:?}",
                descriptor.label, stage.stage
            ))
        })?;
        if sets.len() > 1 {
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{}': only descriptor set 0 is supported; SPIR-V {:?} stage uses sets {:?}",
                descriptor.label,
                stage.stage,
                sets.keys().collect::<Vec<_>>()
            )));
        }
        if let Some(set0) = sets.get(&0) {
            for (&binding, info) in set0 {
                let entry = merged
                    .entry(binding)
                    .or_insert((info.ty, GraphicsShaderStageFlags::NONE));
                if entry.0 != info.ty {
                    return Err(StreamError::GpuError(format!(
                        "Graphics kernel '{}': binding {binding} type conflict — {:?} vs {:?} (in {:?})",
                        descriptor.label, entry.0, info.ty, stage.stage
                    )));
                }
                entry.1 |= stage_flag;
            }
        }
        if let Some(info) = reflection.get_push_constant_range().map_err(|e| {
            StreamError::GpuError(format!(
                "Graphics kernel '{}': failed to read push-constant range for {:?} stage: {e:?}",
                descriptor.label, stage.stage
            ))
        })? {
            spirv_push_size = spirv_push_size.max(info.size);
            spirv_push_stages |= stage_flag;
        }
    }

    // Each declared binding must agree with merged shader declaration.
    for spec in descriptor.bindings {
        let merged_entry = merged.get(&spec.binding).ok_or_else(|| {
            StreamError::GpuError(format!(
                "Graphics kernel '{}': binding {} declared but missing in SPIR-V",
                descriptor.label, spec.binding
            ))
        })?;
        let expected = expected_spirv_type(spec.kind);
        if merged_entry.0 != expected {
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{}': binding {} declared {:?} ({:?}), but SPIR-V has {:?}",
                descriptor.label, spec.binding, spec.kind, expected, merged_entry.0
            )));
        }
        // Declared visibility must cover at least the SPIR-V's stages —
        // i.e., declared stages ⊇ shader stages. A binding consumed in
        // fragment but declared visible only to vertex would be a Vulkan
        // validation error at draw time; reject up front.
        if (merged_entry.1.bits() & !spec.stages.bits()) != 0 {
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{}': binding {} stage visibility mismatch — declared {:?}, SPIR-V uses bits {:#b}",
                descriptor.label,
                spec.binding,
                spec.stages.bits(),
                merged_entry.1.bits()
            )));
        }
    }

    // Conversely, every SPIR-V binding must be declared.
    for (&binding, (ty, stages)) in &merged {
        if !descriptor.bindings.iter().any(|s| s.binding == binding) {
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{}': SPIR-V declares binding {} ({:?}, stages {:#b}) but it is missing from the descriptor",
                descriptor.label, binding, ty, stages.bits()
            )));
        }
    }

    // Push-constant size must match.
    if spirv_push_size != descriptor.push_constants.size {
        return Err(StreamError::GpuError(format!(
            "Graphics kernel '{}': push-constant size mismatch — SPIR-V has {} bytes (stages {:#b}), descriptor declares {} bytes",
            descriptor.label,
            spirv_push_size,
            spirv_push_stages.bits(),
            descriptor.push_constants.size
        )));
    }
    if spirv_push_size > 0 && (spirv_push_stages.bits() & !descriptor.push_constants.stages.bits()) != 0
    {
        return Err(StreamError::GpuError(format!(
            "Graphics kernel '{}': push-constant stage visibility mismatch — declared {:#b}, SPIR-V uses {:#b}",
            descriptor.label,
            descriptor.push_constants.stages.bits(),
            spirv_push_stages.bits()
        )));
    }

    Ok(())
}

fn stage_to_flag(stage: GraphicsShaderStage) -> GraphicsShaderStageFlags {
    match stage {
        GraphicsShaderStage::Vertex => GraphicsShaderStageFlags::VERTEX,
        GraphicsShaderStage::Fragment => GraphicsShaderStageFlags::FRAGMENT,
    }
}

fn expected_spirv_type(kind: GraphicsBindingKind) -> RDescriptorType {
    match kind {
        GraphicsBindingKind::StorageBuffer => RDescriptorType::STORAGE_BUFFER,
        GraphicsBindingKind::UniformBuffer => RDescriptorType::UNIFORM_BUFFER,
        GraphicsBindingKind::SampledTexture => RDescriptorType::COMBINED_IMAGE_SAMPLER,
        GraphicsBindingKind::StorageImage => RDescriptorType::STORAGE_IMAGE,
    }
}

fn descriptor_kind_to_vk(kind: GraphicsBindingKind) -> vk::DescriptorType {
    match kind {
        GraphicsBindingKind::StorageBuffer => vk::DescriptorType::STORAGE_BUFFER,
        GraphicsBindingKind::UniformBuffer => vk::DescriptorType::UNIFORM_BUFFER,
        GraphicsBindingKind::SampledTexture => vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
        GraphicsBindingKind::StorageImage => vk::DescriptorType::STORAGE_IMAGE,
    }
}

fn shader_stage_flags_to_vk(stages: GraphicsShaderStageFlags) -> vk::ShaderStageFlags {
    let mut out = vk::ShaderStageFlags::empty();
    if stages.contains(GraphicsShaderStageFlags::VERTEX) {
        out |= vk::ShaderStageFlags::VERTEX;
    }
    if stages.contains(GraphicsShaderStageFlags::FRAGMENT) {
        out |= vk::ShaderStageFlags::FRAGMENT;
    }
    out
}

fn graphics_stage_to_vk(stage: GraphicsShaderStage) -> vk::ShaderStageFlags {
    match stage {
        GraphicsShaderStage::Vertex => vk::ShaderStageFlags::VERTEX,
        GraphicsShaderStage::Fragment => vk::ShaderStageFlags::FRAGMENT,
    }
}

fn primitive_topology_to_vk(t: PrimitiveTopology) -> vk::PrimitiveTopology {
    match t {
        PrimitiveTopology::PointList => vk::PrimitiveTopology::POINT_LIST,
        PrimitiveTopology::LineList => vk::PrimitiveTopology::LINE_LIST,
        PrimitiveTopology::LineStrip => vk::PrimitiveTopology::LINE_STRIP,
        PrimitiveTopology::TriangleList => vk::PrimitiveTopology::TRIANGLE_LIST,
        PrimitiveTopology::TriangleStrip => vk::PrimitiveTopology::TRIANGLE_STRIP,
        PrimitiveTopology::TriangleFan => vk::PrimitiveTopology::TRIANGLE_FAN,
    }
}

fn polygon_mode_to_vk(m: PolygonMode) -> vk::PolygonMode {
    match m {
        PolygonMode::Fill => vk::PolygonMode::FILL,
        PolygonMode::Line => vk::PolygonMode::LINE,
        PolygonMode::Point => vk::PolygonMode::POINT,
    }
}

fn cull_mode_to_vk(m: CullMode) -> vk::CullModeFlags {
    match m {
        CullMode::None => vk::CullModeFlags::NONE,
        CullMode::Front => vk::CullModeFlags::FRONT,
        CullMode::Back => vk::CullModeFlags::BACK,
        CullMode::FrontAndBack => vk::CullModeFlags::FRONT_AND_BACK,
    }
}

fn front_face_to_vk(f: FrontFace) -> vk::FrontFace {
    match f {
        FrontFace::CounterClockwise => vk::FrontFace::COUNTER_CLOCKWISE,
        FrontFace::Clockwise => vk::FrontFace::CLOCKWISE,
    }
}

fn depth_compare_op_to_vk(c: DepthCompareOp) -> vk::CompareOp {
    match c {
        DepthCompareOp::Never => vk::CompareOp::NEVER,
        DepthCompareOp::Less => vk::CompareOp::LESS,
        DepthCompareOp::Equal => vk::CompareOp::EQUAL,
        DepthCompareOp::LessOrEqual => vk::CompareOp::LESS_OR_EQUAL,
        DepthCompareOp::Greater => vk::CompareOp::GREATER,
        DepthCompareOp::NotEqual => vk::CompareOp::NOT_EQUAL,
        DepthCompareOp::GreaterOrEqual => vk::CompareOp::GREATER_OR_EQUAL,
        DepthCompareOp::Always => vk::CompareOp::ALWAYS,
    }
}

fn blend_factor_to_vk(b: BlendFactor) -> vk::BlendFactor {
    match b {
        BlendFactor::Zero => vk::BlendFactor::ZERO,
        BlendFactor::One => vk::BlendFactor::ONE,
        BlendFactor::SrcColor => vk::BlendFactor::SRC_COLOR,
        BlendFactor::OneMinusSrcColor => vk::BlendFactor::ONE_MINUS_SRC_COLOR,
        BlendFactor::DstColor => vk::BlendFactor::DST_COLOR,
        BlendFactor::OneMinusDstColor => vk::BlendFactor::ONE_MINUS_DST_COLOR,
        BlendFactor::SrcAlpha => vk::BlendFactor::SRC_ALPHA,
        BlendFactor::OneMinusSrcAlpha => vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        BlendFactor::DstAlpha => vk::BlendFactor::DST_ALPHA,
        BlendFactor::OneMinusDstAlpha => vk::BlendFactor::ONE_MINUS_DST_ALPHA,
        BlendFactor::ConstantColor => vk::BlendFactor::CONSTANT_COLOR,
        BlendFactor::OneMinusConstantColor => vk::BlendFactor::ONE_MINUS_CONSTANT_COLOR,
        BlendFactor::ConstantAlpha => vk::BlendFactor::CONSTANT_ALPHA,
        BlendFactor::OneMinusConstantAlpha => vk::BlendFactor::ONE_MINUS_CONSTANT_ALPHA,
        BlendFactor::SrcAlphaSaturate => vk::BlendFactor::SRC_ALPHA_SATURATE,
    }
}

fn blend_op_to_vk(op: BlendOp) -> vk::BlendOp {
    match op {
        BlendOp::Add => vk::BlendOp::ADD,
        BlendOp::Subtract => vk::BlendOp::SUBTRACT,
        BlendOp::ReverseSubtract => vk::BlendOp::REVERSE_SUBTRACT,
        BlendOp::Min => vk::BlendOp::MIN,
        BlendOp::Max => vk::BlendOp::MAX,
    }
}

fn color_write_mask_to_vk(m: ColorWriteMask) -> vk::ColorComponentFlags {
    let mut out = vk::ColorComponentFlags::empty();
    if (m.bits() & ColorWriteMask::R.bits()) != 0 {
        out |= vk::ColorComponentFlags::R;
    }
    if (m.bits() & ColorWriteMask::G.bits()) != 0 {
        out |= vk::ColorComponentFlags::G;
    }
    if (m.bits() & ColorWriteMask::B.bits()) != 0 {
        out |= vk::ColorComponentFlags::B;
    }
    if (m.bits() & ColorWriteMask::A.bits()) != 0 {
        out |= vk::ColorComponentFlags::A;
    }
    out
}

fn vertex_attribute_format_to_vk(f: VertexAttributeFormat) -> vk::Format {
    match f {
        VertexAttributeFormat::R32Float => vk::Format::R32_SFLOAT,
        VertexAttributeFormat::Rg32Float => vk::Format::R32G32_SFLOAT,
        VertexAttributeFormat::Rgb32Float => vk::Format::R32G32B32_SFLOAT,
        VertexAttributeFormat::Rgba32Float => vk::Format::R32G32B32A32_SFLOAT,
        VertexAttributeFormat::R32Uint => vk::Format::R32_UINT,
        VertexAttributeFormat::Rg32Uint => vk::Format::R32G32_UINT,
        VertexAttributeFormat::Rgb32Uint => vk::Format::R32G32B32_UINT,
        VertexAttributeFormat::Rgba32Uint => vk::Format::R32G32B32A32_UINT,
        VertexAttributeFormat::R32Sint => vk::Format::R32_SINT,
        VertexAttributeFormat::Rg32Sint => vk::Format::R32G32_SINT,
        VertexAttributeFormat::Rgb32Sint => vk::Format::R32G32B32_SINT,
        VertexAttributeFormat::Rgba32Sint => vk::Format::R32G32B32A32_SINT,
        VertexAttributeFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        VertexAttributeFormat::Rgba8Snorm => vk::Format::R8G8B8A8_SNORM,
    }
}

fn vertex_input_rate_to_vk(r: VertexInputRate) -> vk::VertexInputRate {
    match r {
        VertexInputRate::Vertex => vk::VertexInputRate::VERTEX,
        VertexInputRate::Instance => vk::VertexInputRate::INSTANCE,
    }
}

fn texture_format_to_vk(f: TextureFormat) -> vk::Format {
    match f {
        TextureFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        TextureFormat::Rgba8UnormSrgb => vk::Format::R8G8B8A8_SRGB,
        TextureFormat::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        TextureFormat::Bgra8UnormSrgb => vk::Format::B8G8R8A8_SRGB,
        TextureFormat::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
        TextureFormat::Rgba32Float => vk::Format::R32G32B32A32_SFLOAT,
        // NV12 isn't a valid color attachment; we leave the mapping for
        // sampling consumers and refuse it as an attachment format below.
        TextureFormat::Nv12 => vk::Format::G8_B8R8_2PLANE_420_UNORM,
    }
}

fn depth_format_to_vk(f: DepthFormat) -> vk::Format {
    match f {
        DepthFormat::D16Unorm => vk::Format::D16_UNORM,
        DepthFormat::D32Sfloat => vk::Format::D32_SFLOAT,
        DepthFormat::D24UnormS8Uint => vk::Format::D24_UNORM_S8_UINT,
    }
}

fn index_type_to_vk(t: IndexType) -> vk::IndexType {
    match t {
        IndexType::Uint16 => vk::IndexType::UINT16,
        IndexType::Uint32 => vk::IndexType::UINT32,
    }
}

fn create_shader_modules(
    device: &vulkanalia::Device,
    descriptor: &GraphicsKernelDescriptor<'_>,
) -> Result<Vec<(GraphicsShaderStage, vk::ShaderModule)>> {
    let mut out: Vec<(GraphicsShaderStage, vk::ShaderModule)> =
        Vec::with_capacity(descriptor.stages.len());
    for stage in descriptor.stages {
        let spirv: Vec<u32> = stage
            .spv
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let info = vk::ShaderModuleCreateInfo::builder().code(&spirv).build();
        match unsafe { device.create_shader_module(&info, None) } {
            Ok(m) => out.push((stage.stage, m)),
            Err(e) => {
                destroy_shader_modules(device, &out);
                return Err(StreamError::GpuError(format!(
                    "Graphics kernel '{}': failed to create {:?} shader module: {e}",
                    descriptor.label, stage.stage
                )));
            }
        }
    }
    Ok(out)
}

fn destroy_shader_modules(
    device: &vulkanalia::Device,
    modules: &[(GraphicsShaderStage, vk::ShaderModule)],
) {
    for (_, m) in modules {
        unsafe { device.destroy_shader_module(*m, None) };
    }
}

fn create_descriptor_set_layout(
    device: &vulkanalia::Device,
    bindings: &[GraphicsBindingSpec],
) -> Result<vk::DescriptorSetLayout> {
    let layout_bindings: Vec<vk::DescriptorSetLayoutBinding> = bindings
        .iter()
        .map(|spec| {
            vk::DescriptorSetLayoutBinding::builder()
                .binding(spec.binding)
                .descriptor_type(descriptor_kind_to_vk(spec.kind))
                .descriptor_count(1)
                .stage_flags(shader_stage_flags_to_vk(spec.stages))
                .build()
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::builder()
        .bindings(&layout_bindings)
        .build();
    unsafe { device.create_descriptor_set_layout(&info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create descriptor set layout: {e}")))
}

fn create_pipeline_layout(
    device: &vulkanalia::Device,
    set_layout: vk::DescriptorSetLayout,
    push: crate::core::rhi::GraphicsPushConstants,
) -> Result<vk::PipelineLayout> {
    let set_layouts = [set_layout];
    let push_ranges: Vec<vk::PushConstantRange> = if push.size > 0 {
        vec![vk::PushConstantRange::builder()
            .stage_flags(shader_stage_flags_to_vk(push.stages))
            .offset(0)
            .size(push.size)
            .build()]
    } else {
        Vec::new()
    };
    let info = vk::PipelineLayoutCreateInfo::builder()
        .set_layouts(&set_layouts)
        .push_constant_ranges(&push_ranges)
        .build();
    unsafe { device.create_pipeline_layout(&info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create pipeline layout: {e}")))
}

fn create_descriptor_pool(
    device: &vulkanalia::Device,
    bindings: &[GraphicsBindingSpec],
    sets_in_flight: u32,
) -> Result<vk::DescriptorPool> {
    let mut counts: HashMap<vk::DescriptorType, u32> = HashMap::new();
    for spec in bindings {
        *counts.entry(descriptor_kind_to_vk(spec.kind)).or_insert(0) += sets_in_flight;
    }
    let pool_sizes: Vec<vk::DescriptorPoolSize> = counts
        .into_iter()
        .map(|(ty, count)| {
            vk::DescriptorPoolSize::builder()
                .type_(ty)
                .descriptor_count(count.max(1))
                .build()
        })
        .collect();
    let info = vk::DescriptorPoolCreateInfo::builder()
        .max_sets(sets_in_flight)
        .pool_sizes(&pool_sizes)
        .build();
    unsafe { device.create_descriptor_pool(&info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create descriptor pool: {e}")))
}

fn allocate_descriptor_sets(
    device: &vulkanalia::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
    count: u32,
) -> Result<Vec<vk::DescriptorSet>> {
    let layouts: Vec<vk::DescriptorSetLayout> = (0..count).map(|_| layout).collect();
    let info = vk::DescriptorSetAllocateInfo::builder()
        .descriptor_pool(pool)
        .set_layouts(&layouts)
        .build();
    let sets = unsafe { device.allocate_descriptor_sets(&info) }
        .map_err(|e| StreamError::GpuError(format!("Failed to allocate descriptor sets: {e}")))?;
    Ok(sets)
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
        .map_err(|e| StreamError::GpuError(format!("Failed to create command pool: {e}")))
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
        .map_err(|e| StreamError::GpuError(format!("Failed to allocate command buffer: {e}")))?;
    Ok(buffers[0])
}

fn vk_buffer_for(buffer: &RhiPixelBuffer) -> (vk::Buffer, vk::DeviceSize) {
    let inner = &buffer.buffer_ref().inner;
    (inner.buffer(), inner.size())
}

fn create_graphics_pipeline_with_cache(
    device: &vulkanalia::Device,
    shader_modules: &[(GraphicsShaderStage, vk::ShaderModule)],
    stages: &[GraphicsStage<'_>],
    pipeline_layout: vk::PipelineLayout,
    state: &GraphicsPipelineState,
    label: &str,
) -> Result<vk::Pipeline> {
    // Reject NV12 as a color attachment (planar-format output is not a
    // graphics-pipeline target).
    for (i, fmt) in state.attachment_formats.color.iter().enumerate() {
        if matches!(fmt, TextureFormat::Nv12) {
            return Err(StreamError::GpuError(format!(
                "Graphics kernel '{label}': color attachment {i} format {fmt:?} is not a valid render target"
            )));
        }
    }
    if state.multisample.samples != 1 {
        return Err(StreamError::GpuError(format!(
            "Graphics kernel '{label}': multisample.samples = {} is not supported (only 1)",
            state.multisample.samples
        )));
    }

    // Hash the SPIR-V across all stages to key the pipeline cache.
    let cache_key = hash_stages(stages);
    let cache_path = pipeline_cache_file_path(&cache_key);
    let initial_data = cache_path.as_deref().and_then(read_cache_blob);
    let pipeline_cache = create_pipeline_cache_handle(device, initial_data.as_deref(), label);
    let cache_handle = pipeline_cache.unwrap_or(vk::PipelineCache::null());

    // Shader stages — match `shader_modules` order to `stages` order.
    let entry_names: Vec<std::ffi::CString> = stages
        .iter()
        .map(|s| std::ffi::CString::new(s.entry_point).expect("entry point must be ASCII"))
        .collect();
    let shader_stage_infos: Vec<vk::PipelineShaderStageCreateInfo> = stages
        .iter()
        .zip(shader_modules.iter())
        .zip(entry_names.iter())
        .map(|((stage_desc, (_, module)), name)| {
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(graphics_stage_to_vk(stage_desc.stage))
                .module(*module)
                .name(name.as_bytes_with_nul())
                .build()
        })
        .collect();

    // Vertex input.
    let (vertex_bindings, vertex_attributes) = match &state.vertex_input {
        VertexInputState::None => (Vec::new(), Vec::new()),
        VertexInputState::Buffers {
            bindings,
            attributes,
        } => {
            let vb: Vec<vk::VertexInputBindingDescription> = bindings
                .iter()
                .map(|b| {
                    vk::VertexInputBindingDescription::builder()
                        .binding(b.binding)
                        .stride(b.stride)
                        .input_rate(vertex_input_rate_to_vk(b.input_rate))
                        .build()
                })
                .collect();
            let va: Vec<vk::VertexInputAttributeDescription> = attributes
                .iter()
                .map(|a| {
                    vk::VertexInputAttributeDescription::builder()
                        .location(a.location)
                        .binding(a.binding)
                        .format(vertex_attribute_format_to_vk(a.format))
                        .offset(a.offset)
                        .build()
                })
                .collect();
            (vb, va)
        }
    };
    let vertex_input_info = vk::PipelineVertexInputStateCreateInfo::builder()
        .vertex_binding_descriptions(&vertex_bindings)
        .vertex_attribute_descriptions(&vertex_attributes)
        .build();

    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::builder()
        .topology(primitive_topology_to_vk(state.topology))
        .primitive_restart_enable(false)
        .build();

    // Viewport state — declare 1 viewport + 1 scissor; either dynamic or
    // baked.
    let baked_viewport = [vk::Viewport::default()];
    let baked_scissor = [vk::Rect2D::default()];
    let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
        .viewports(&baked_viewport)
        .scissors(&baked_scissor)
        .build();

    let rasterizer = vk::PipelineRasterizationStateCreateInfo::builder()
        .polygon_mode(polygon_mode_to_vk(state.rasterization.polygon_mode))
        .cull_mode(cull_mode_to_vk(state.rasterization.cull_mode))
        .front_face(front_face_to_vk(state.rasterization.front_face))
        .line_width(state.rasterization.line_width)
        .depth_clamp_enable(false)
        .rasterizer_discard_enable(false)
        .depth_bias_enable(false)
        .build();

    let multisampling = vk::PipelineMultisampleStateCreateInfo::builder()
        .rasterization_samples(vk::SampleCountFlags::_1)
        .sample_shading_enable(false)
        .build();

    let depth_stencil_info = build_depth_stencil_info(&state.depth_stencil);

    let color_blend_attachments = build_color_blend_attachments(
        &state.color_blend,
        state.attachment_formats.color.len(),
    );
    let color_blend_state = vk::PipelineColorBlendStateCreateInfo::builder()
        .logic_op_enable(false)
        .logic_op(vk::LogicOp::COPY)
        .attachments(&color_blend_attachments)
        .blend_constants([0.0, 0.0, 0.0, 0.0])
        .build();

    let dynamic_states_buf: Vec<vk::DynamicState> = match state.dynamic_state {
        GraphicsDynamicState::None => Vec::new(),
        GraphicsDynamicState::ViewportScissor => {
            vec![vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR]
        }
    };
    let dynamic_state_info = vk::PipelineDynamicStateCreateInfo::builder()
        .dynamic_states(&dynamic_states_buf)
        .build();

    // Dynamic rendering: pNext on graphics pipeline create info.
    let color_attachment_formats: Vec<vk::Format> = state
        .attachment_formats
        .color
        .iter()
        .map(|f| texture_format_to_vk(*f))
        .collect();
    let mut rendering_info_builder = vk::PipelineRenderingCreateInfo::builder()
        .color_attachment_formats(&color_attachment_formats);
    if let Some(df) = state.attachment_formats.depth {
        rendering_info_builder = rendering_info_builder.depth_attachment_format(depth_format_to_vk(df));
    }
    let mut pipeline_rendering_info = rendering_info_builder.build();

    let mut pipeline_info_builder = vk::GraphicsPipelineCreateInfo::builder()
        .stages(&shader_stage_infos)
        .vertex_input_state(&vertex_input_info)
        .input_assembly_state(&input_assembly)
        .viewport_state(&viewport_state)
        .rasterization_state(&rasterizer)
        .multisample_state(&multisampling)
        .color_blend_state(&color_blend_state)
        .dynamic_state(&dynamic_state_info)
        .layout(pipeline_layout)
        .push_next(&mut pipeline_rendering_info);
    if matches!(state.depth_stencil, DepthStencilState::Enabled { .. }) {
        pipeline_info_builder = pipeline_info_builder.depth_stencil_state(&depth_stencil_info);
    }
    let pipeline_info = pipeline_info_builder.build();

    let pipelines_result =
        unsafe { device.create_graphics_pipelines(cache_handle, &[pipeline_info], None) };

    if pipeline_cache.is_some() {
        if let Some(path) = cache_path.as_deref() {
            persist_pipeline_cache(device, cache_handle, path, label);
        }
        unsafe { device.destroy_pipeline_cache(cache_handle, None) };
    }

    let pipelines = pipelines_result.map_err(|e| {
        StreamError::GpuError(format!(
            "Graphics kernel '{label}': failed to create graphics pipeline: {e}"
        ))
    })?;
    Ok(pipelines.0[0])
}

fn build_depth_stencil_info(
    state: &DepthStencilState,
) -> vk::PipelineDepthStencilStateCreateInfo {
    match state {
        DepthStencilState::Disabled => vk::PipelineDepthStencilStateCreateInfo::builder()
            .depth_test_enable(false)
            .depth_write_enable(false)
            .depth_compare_op(vk::CompareOp::ALWAYS)
            .depth_bounds_test_enable(false)
            .stencil_test_enable(false)
            .build(),
        DepthStencilState::Enabled {
            depth_test,
            depth_write,
        } => vk::PipelineDepthStencilStateCreateInfo::builder()
            .depth_test_enable(true)
            .depth_write_enable(*depth_write)
            .depth_compare_op(depth_compare_op_to_vk(*depth_test))
            .depth_bounds_test_enable(false)
            .stencil_test_enable(false)
            .build(),
    }
}

fn build_color_blend_attachments(
    state: &ColorBlendState,
    count: usize,
) -> Vec<vk::PipelineColorBlendAttachmentState> {
    let template = match state {
        ColorBlendState::Disabled { color_write_mask } => {
            vk::PipelineColorBlendAttachmentState::builder()
                .blend_enable(false)
                .src_color_blend_factor(vk::BlendFactor::ONE)
                .dst_color_blend_factor(vk::BlendFactor::ZERO)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ONE)
                .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
                .alpha_blend_op(vk::BlendOp::ADD)
                .color_write_mask(color_write_mask_to_vk(*color_write_mask))
                .build()
        }
        ColorBlendState::Enabled(att) => vk::PipelineColorBlendAttachmentState::builder()
            .blend_enable(true)
            .src_color_blend_factor(blend_factor_to_vk(att.src_color_blend_factor))
            .dst_color_blend_factor(blend_factor_to_vk(att.dst_color_blend_factor))
            .color_blend_op(blend_op_to_vk(att.color_blend_op))
            .src_alpha_blend_factor(blend_factor_to_vk(att.src_alpha_blend_factor))
            .dst_alpha_blend_factor(blend_factor_to_vk(att.dst_alpha_blend_factor))
            .alpha_blend_op(blend_op_to_vk(att.alpha_blend_op))
            .color_write_mask(color_write_mask_to_vk(att.color_write_mask))
            .build(),
    };
    (0..count.max(1)).map(|_| template).collect()
}

// ---- Pipeline cache helpers (mirror compute kernel) -------------------------

fn hash_stages(stages: &[GraphicsStage<'_>]) -> String {
    let mut hasher = Sha256::new();
    for stage in stages {
        // Mix in the stage discriminant so two kernels with the same SPIR-V
        // bytes but different stage classification get distinct cache keys.
        hasher.update(match stage.stage {
            GraphicsShaderStage::Vertex => b"V".as_ref(),
            GraphicsShaderStage::Fragment => b"F".as_ref(),
        });
        hasher.update(stage.entry_point.as_bytes());
        hasher.update((stage.spv.len() as u64).to_le_bytes());
        hasher.update(stage.spv);
    }
    format!("{:x}", hasher.finalize())
}

fn pipeline_cache_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(PIPELINE_CACHE_DIR_ENV) {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    dirs::cache_dir().map(|d| d.join("streamlib/pipeline-cache"))
}

fn pipeline_cache_file_path(hash_hex: &str) -> Option<PathBuf> {
    let dir = pipeline_cache_dir()?;
    Some(dir.join(format!("{hash_hex}.gfx.bin")))
}

fn read_cache_blob(path: &Path) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) if !bytes.is_empty() => Some(bytes),
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(
                "graphics pipeline cache: unreadable cache file at {}: {e}",
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
            "Graphics kernel '{label}': loading pipeline cache (pInitialData {} bytes)",
            data.len()
        );
    }
    let info = info.build();
    match unsafe { device.create_pipeline_cache(&info, None) } {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::warn!(
                "Graphics kernel '{label}': vkCreatePipelineCache failed: {e} — falling back to null cache"
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
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                "Graphics kernel '{label}': vkGetPipelineCacheData failed: {e}"
            );
            return;
        }
    };
    if data.is_empty() {
        return;
    }
    if let Err(e) = atomic_write_pipeline_cache(path, &data) {
        tracing::warn!(
            "Graphics kernel '{label}': failed to persist pipeline cache to {}: {e}",
            path.display()
        );
    } else {
        tracing::debug!(
            "Graphics kernel '{label}': persisted pipeline cache ({} bytes) to {}",
            data.len(),
            path.display()
        );
    }
}

fn atomic_write_pipeline_cache(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let suffix = format!(
        "tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let mut tmp = path.to_path_buf();
    tmp.set_extension(format!("gfx.bin.{suffix}"));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::{
        AttachmentFormats, ColorBlendState, ColorWriteMask, DepthCompareOp, DepthStencilState,
        GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor,
        GraphicsPipelineState, GraphicsPushConstants, GraphicsShaderStageFlags, GraphicsStage,
        MultisampleState, PolygonMode, PrimitiveTopology, RasterizationState, TextureFormat,
        VertexAttributeFormat, VertexInputAttribute, VertexInputBinding, VertexInputRate,
        VertexInputState, derive_bindings_from_spirv_multistage,
    };

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(d) => Some(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                None
            }
        }
    }

    fn vert_spv() -> &'static [u8] {
        include_bytes!(concat!(env!("OUT_DIR"), "/display_blit.vert.spv"))
    }
    fn frag_spv() -> &'static [u8] {
        include_bytes!(concat!(env!("OUT_DIR"), "/display_blit.frag.spv"))
    }

    fn default_pipeline_state() -> GraphicsPipelineState {
        GraphicsPipelineState {
            topology: PrimitiveTopology::TriangleList,
            vertex_input: VertexInputState::None,
            rasterization: RasterizationState::default(),
            multisample: MultisampleState::default(),
            depth_stencil: DepthStencilState::Disabled,
            color_blend: ColorBlendState::Disabled {
                color_write_mask: ColorWriteMask::RGBA,
            },
            attachment_formats: AttachmentFormats::color_only(TextureFormat::Bgra8Unorm),
            dynamic_state: GraphicsDynamicState::ViewportScissor,
        }
    }

    fn display_blit_descriptor<'a>(
        stages: &'a [GraphicsStage<'a>],
        bindings: &'a [GraphicsBindingSpec],
        pipeline_state: &'a GraphicsPipelineState,
    ) -> GraphicsKernelDescriptor<'a> {
        GraphicsKernelDescriptor {
            label: "display-blit",
            stages,
            bindings,
            push_constants: GraphicsPushConstants {
                size: 16,
                stages: GraphicsShaderStageFlags::FRAGMENT,
            },
            pipeline_state: pipeline_state.clone(),
            descriptor_sets_in_flight: 2,
        }
    }

    // ---- Multi-stage SPIR-V reflection ------------------------------------

    #[test]
    fn derives_bindings_from_display_blit_stages() {
        let stages = [
            GraphicsStage::vertex(vert_spv()),
            GraphicsStage::fragment(frag_spv()),
        ];
        let (bindings, push) = derive_bindings_from_spirv_multistage(&stages)
            .expect("derive bindings");
        assert_eq!(bindings.len(), 1, "display_blit declares one fragment binding");
        assert_eq!(bindings[0].binding, 0);
        assert_eq!(bindings[0].kind, GraphicsBindingKind::SampledTexture);
        assert!(
            bindings[0].stages.contains(GraphicsShaderStageFlags::FRAGMENT),
            "binding 0 is consumed by fragment stage"
        );
        assert_eq!(push.size, 16, "push constants are scale (vec2) + offset (vec2)");
        assert!(push.stages.contains(GraphicsShaderStageFlags::FRAGMENT));
    }

    // ---- Validation rejections (host-only, no GPU device required) --------

    #[test]
    fn rejects_descriptor_with_mismatched_binding_kind() {
        // SPIR-V binding 0 is SampledTexture; declaring it as StorageBuffer must fail.
        let bindings = [GraphicsBindingSpec::storage_buffer(
            0,
            GraphicsShaderStageFlags::FRAGMENT,
        )];
        let stages = [
            GraphicsStage::vertex(vert_spv()),
            GraphicsStage::fragment(frag_spv()),
        ];
        let pipeline_state = default_pipeline_state();
        let descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        let err = validate_against_spirv(&descriptor)
            .err()
            .expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("binding 0") && (msg.contains("StorageBuffer") || msg.contains("STORAGE_BUFFER")),
            "expected mismatch error mentioning binding 0 and StorageBuffer, got: {msg}"
        );
    }

    #[test]
    fn rejects_descriptor_with_extra_binding_not_in_shader() {
        // display_blit declares only binding 0; extra binding 1 must fail.
        let bindings = [
            GraphicsBindingSpec::sampled_texture(0, GraphicsShaderStageFlags::FRAGMENT),
            GraphicsBindingSpec::sampled_texture(1, GraphicsShaderStageFlags::FRAGMENT),
        ];
        let stages = [
            GraphicsStage::vertex(vert_spv()),
            GraphicsStage::fragment(frag_spv()),
        ];
        let pipeline_state = default_pipeline_state();
        let descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        let err = validate_against_spirv(&descriptor)
            .err()
            .expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("binding 1"),
            "expected error about binding 1, got: {msg}"
        );
    }

    #[test]
    fn rejects_descriptor_missing_a_binding_the_shader_declares() {
        // Empty bindings ⇒ SPIR-V's declared binding 0 is missing.
        let bindings: [GraphicsBindingSpec; 0] = [];
        let stages = [
            GraphicsStage::vertex(vert_spv()),
            GraphicsStage::fragment(frag_spv()),
        ];
        let pipeline_state = default_pipeline_state();
        let descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        let err = validate_against_spirv(&descriptor)
            .err()
            .expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("binding 0") && msg.contains("missing"),
            "expected error about missing binding 0, got: {msg}"
        );
    }

    #[test]
    fn rejects_push_constant_size_mismatch() {
        let bindings = [GraphicsBindingSpec::sampled_texture(
            0,
            GraphicsShaderStageFlags::FRAGMENT,
        )];
        let stages = [
            GraphicsStage::vertex(vert_spv()),
            GraphicsStage::fragment(frag_spv()),
        ];
        let pipeline_state = default_pipeline_state();
        let mut descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        descriptor.push_constants = GraphicsPushConstants {
            size: 64, // SPIR-V declares 16
            stages: GraphicsShaderStageFlags::FRAGMENT,
        };
        let err = validate_against_spirv(&descriptor)
            .err()
            .expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("push-constant size mismatch"),
            "expected push-constant size error, got: {msg}"
        );
    }

    #[test]
    fn rejects_descriptor_with_stage_visibility_too_narrow() {
        // SPIR-V uses binding 0 in fragment; declaring stages = VERTEX must fail.
        let bindings = [GraphicsBindingSpec::sampled_texture(
            0,
            GraphicsShaderStageFlags::VERTEX,
        )];
        let stages = [
            GraphicsStage::vertex(vert_spv()),
            GraphicsStage::fragment(frag_spv()),
        ];
        let pipeline_state = default_pipeline_state();
        let descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        let err = validate_against_spirv(&descriptor)
            .err()
            .expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("binding 0") && msg.contains("stage visibility"),
            "expected stage visibility error for binding 0, got: {msg}"
        );
    }

    #[test]
    fn new_rejects_zero_descriptor_sets_in_flight() {
        // descriptor_sets_in_flight = 0 must fail before any Vulkan call.
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let bindings = [GraphicsBindingSpec::sampled_texture(
            0,
            GraphicsShaderStageFlags::FRAGMENT,
        )];
        let stages = [
            GraphicsStage::vertex(vert_spv()),
            GraphicsStage::fragment(frag_spv()),
        ];
        let pipeline_state = default_pipeline_state();
        let mut descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        descriptor.descriptor_sets_in_flight = 0;
        let err = VulkanGraphicsKernel::new(&device, &descriptor)
            .err()
            .expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("descriptor_sets_in_flight"),
            "expected zero-ring-depth error, got: {msg}"
        );
    }

    #[test]
    fn new_rejects_descriptor_without_vertex_stage() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let bindings = [GraphicsBindingSpec::sampled_texture(
            0,
            GraphicsShaderStageFlags::FRAGMENT,
        )];
        let stages = [GraphicsStage::fragment(frag_spv())]; // no vertex
        let pipeline_state = default_pipeline_state();
        let descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        let err = VulkanGraphicsKernel::new(&device, &descriptor)
            .err()
            .expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("Vertex stage"),
            "expected error about missing Vertex stage, got: {msg}"
        );
    }

    #[test]
    fn new_rejects_descriptor_without_fragment_stage() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let bindings: [GraphicsBindingSpec; 0] = [];
        let stages = [GraphicsStage::vertex(vert_spv())]; // no fragment
        let pipeline_state = default_pipeline_state();
        let mut descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        descriptor.push_constants = GraphicsPushConstants::NONE;
        let err = VulkanGraphicsKernel::new(&device, &descriptor)
            .err()
            .expect("expected validation failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("Fragment stage"),
            "expected error about missing Fragment stage, got: {msg}"
        );
    }

    // ---- Pipeline construction (GPU device required) ----------------------

    #[test]
    fn constructs_display_blit_kernel() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let bindings = [GraphicsBindingSpec::sampled_texture(
            0,
            GraphicsShaderStageFlags::FRAGMENT,
        )];
        let stages = [
            GraphicsStage::vertex(vert_spv()),
            GraphicsStage::fragment(frag_spv()),
        ];
        let pipeline_state = default_pipeline_state();
        let descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        let kernel = VulkanGraphicsKernel::new(&device, &descriptor)
            .expect("kernel must construct");
        assert_eq!(kernel.bindings().len(), 1);
        assert_eq!(kernel.push_constant_size(), 16);
        assert_eq!(kernel.descriptor_sets_in_flight(), 2);
    }

    #[test]
    fn constructs_kernel_with_depth_stencil_enabled() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let bindings = [GraphicsBindingSpec::sampled_texture(
            0,
            GraphicsShaderStageFlags::FRAGMENT,
        )];
        let stages = [
            GraphicsStage::vertex(vert_spv()),
            GraphicsStage::fragment(frag_spv()),
        ];
        let mut pipeline_state = default_pipeline_state();
        pipeline_state.depth_stencil = DepthStencilState::Enabled {
            depth_test: DepthCompareOp::LessOrEqual,
            depth_write: true,
        };
        pipeline_state.attachment_formats = AttachmentFormats {
            color: vec![TextureFormat::Bgra8Unorm],
            depth: Some(crate::core::rhi::DepthFormat::D32Sfloat),
        };
        let descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        // Lock the depth-stencil pipeline-creation path: construction must
        // succeed and the kernel must report its declared shape correctly.
        // Driver bugs in depth-state threading would surface here.
        let kernel = VulkanGraphicsKernel::new(&device, &descriptor)
            .expect("depth-stencil kernel must construct");
        assert_eq!(kernel.bindings().len(), 1);
    }

    #[test]
    fn constructs_kernel_with_alpha_blending() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let bindings = [GraphicsBindingSpec::sampled_texture(
            0,
            GraphicsShaderStageFlags::FRAGMENT,
        )];
        let stages = [
            GraphicsStage::vertex(vert_spv()),
            GraphicsStage::fragment(frag_spv()),
        ];
        let mut pipeline_state = default_pipeline_state();
        pipeline_state.color_blend =
            ColorBlendState::Enabled(crate::core::rhi::ColorBlendAttachment::ALPHA_OVER);
        let descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        let _ = VulkanGraphicsKernel::new(&device, &descriptor)
            .expect("alpha-blending kernel must construct");
    }

    #[test]
    fn constructs_kernel_with_vertex_input_buffers() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        // A pipeline that *declares* vertex input buffers — the display_blit
        // shader doesn't read them, but Vulkan permits unused vertex input
        // declarations. We're locking the pipeline-creation path here, not
        // shader-vs-input correctness.
        let bindings = [GraphicsBindingSpec::sampled_texture(
            0,
            GraphicsShaderStageFlags::FRAGMENT,
        )];
        let stages = [
            GraphicsStage::vertex(vert_spv()),
            GraphicsStage::fragment(frag_spv()),
        ];
        let mut pipeline_state = default_pipeline_state();
        pipeline_state.vertex_input = VertexInputState::Buffers {
            bindings: vec![VertexInputBinding {
                binding: 0,
                stride: 32,
                input_rate: VertexInputRate::Vertex,
            }],
            attributes: vec![
                VertexInputAttribute {
                    location: 1, // location 0 is the vertex shader's gl_VertexIndex-derived UV
                    binding: 0,
                    format: VertexAttributeFormat::Rgb32Float,
                    offset: 0,
                },
                VertexInputAttribute {
                    location: 2,
                    binding: 0,
                    format: VertexAttributeFormat::Rg32Float,
                    offset: 12,
                },
            ],
        };
        let descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        let _ = VulkanGraphicsKernel::new(&device, &descriptor)
            .expect("vertex-input kernel must construct");
    }

    #[test]
    fn frame_index_out_of_range_fails_loud() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let bindings = [GraphicsBindingSpec::sampled_texture(
            0,
            GraphicsShaderStageFlags::FRAGMENT,
        )];
        let stages = [
            GraphicsStage::vertex(vert_spv()),
            GraphicsStage::fragment(frag_spv()),
        ];
        let pipeline_state = default_pipeline_state();
        let descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        let kernel = VulkanGraphicsKernel::new(&device, &descriptor)
            .expect("kernel construction");

        // Allocate a dummy texture for the call. Doesn't matter what it is —
        // the frame_index check happens before the texture is dereferenced.
        let dummy_desc = crate::core::rhi::TextureDescriptor::new(
            16,
            16,
            TextureFormat::Bgra8Unorm,
        )
        .with_usage(
            crate::core::rhi::TextureUsages::TEXTURE_BINDING
                | crate::core::rhi::TextureUsages::COPY_DST,
        );
        let texture =
            crate::vulkan::rhi::HostVulkanTexture::new_device_local(&device, &dummy_desc)
                .expect("test texture allocation");
        let stream_texture = crate::core::rhi::StreamTexture::from_vulkan(texture);

        // Ring depth is 2; index 2 must fail.
        let err = kernel
            .set_sampled_texture(2, 0, &stream_texture)
            .err()
            .expect("expected out-of-range error");
        let msg = format!("{err}");
        assert!(
            msg.contains("frame_index 2 out of range"),
            "expected out-of-range error, got: {msg}"
        );
    }

    #[test]
    fn dispatch_without_setting_bindings_fails_loud() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let bindings = [GraphicsBindingSpec::sampled_texture(
            0,
            GraphicsShaderStageFlags::FRAGMENT,
        )];
        let stages = [
            GraphicsStage::vertex(vert_spv()),
            GraphicsStage::fragment(frag_spv()),
        ];
        let pipeline_state = default_pipeline_state();
        let descriptor = display_blit_descriptor(&stages, &bindings, &pipeline_state);
        let kernel = VulkanGraphicsKernel::new(&device, &descriptor)
            .expect("kernel construction");
        // Don't bind anything before drawing — must fail with a clear error,
        // not corrupt GPU state.
        let draw = crate::core::rhi::DrawCall {
            vertex_count: 3,
            instance_count: 1,
            first_vertex: 0,
            first_instance: 0,
            viewport: Some(Viewport::full(16, 16)),
            scissor: Some(ScissorRect::full(16, 16)),
        };
        let err = kernel
            .cmd_bind_and_draw(vk::CommandBuffer::null(), 0, &draw)
            .err()
            .expect("expected missing-binding error");
        let msg = format!("{err}");
        assert!(
            msg.contains("not set before draw"),
            "expected missing-binding error, got: {msg}"
        );
    }
}
