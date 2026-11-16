// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{
    DrawCall, DrawIndexedCall, GraphicsBindingKind, GraphicsDynamicState, ScissorRect,
    VertexInputState, Viewport,
};
use crate::core::{Error, Result};

use super::{
    BindingResource, OffscreenColorTarget, OffscreenDraw, OffscreenScaffold, PendingState,
    VulkanGraphicsKernelInner,
};

use super::pipeline_creation::{allocate_command_buffer, create_command_pool};

impl VulkanGraphicsKernelInner {
    /// Render into one or more offscreen color attachments using slot 0 of
    /// the descriptor ring. Owns its own command buffer and fence; submits,
    /// waits, and returns. Convenience for unit tests and one-shot
    /// renderers.
    pub(crate) fn offscreen_render(
        &self,
        frame_index: u32,
        color_targets: &[OffscreenColorTarget<'_>],
        extent: (u32, u32),
        draw: OffscreenDraw,
    ) -> Result<()> {
        if color_targets.is_empty() {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{}': offscreen_render called with no color targets",
                self.label
            )));
        }
        let scaffold_handles = self.offscreen_scaffold()?;
        let (command_buffer, fence) = (scaffold_handles.command_buffer, scaffold_handles.fence);

        let (width, height) = extent;
        let viewport = Viewport::full(width, height);
        let scissor = ScissorRect::full(width, height);

        unsafe {
            self.device
                .wait_for_fences(&[fence], true, u64::MAX)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "Graphics kernel '{}': wait_for_fences failed: {e}",
                        self.label
                    ))
                })?;
            self.device.reset_fences(&[fence]).map_err(|e| {
                Error::GpuError(format!(
                    "Graphics kernel '{}': reset_fences failed: {e}",
                    self.label
                ))
            })?;

            self.device
                .reset_command_buffer(command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| {
                    Error::GpuError(format!(
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
                    Error::GpuError(format!(
                        "Graphics kernel '{}': begin_command_buffer failed: {e}",
                        self.label
                    ))
                })?;

            // An earlier submission on this queue can still be touching a
            // target — a pool slot can be handed back while a reader's submit
            // is still in flight — so the transition waits on every prior
            // stage and write.
            let mut barriers: Vec<vk::ImageMemoryBarrier2> =
                Vec::with_capacity(color_targets.len());
            use crate::host_rhi::HostTextureExt;
            for target in color_targets {
                let image = target.texture.vulkan_inner().image().ok_or_else(|| {
                    Error::GpuError(format!(
                        "Graphics kernel '{}': offscreen color target has no VkImage",
                        self.label
                    ))
                })?;
                barriers.push(
                    vk::ImageMemoryBarrier2::builder()
                        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
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
                    let view = t
                        .texture
                        .vulkan_inner()
                        .image_view()
                        .unwrap_or(vk::ImageView::null());
                    let mut attachment = vk::RenderingAttachmentInfo::builder()
                        .image_view(view)
                        .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                        .store_op(vk::AttachmentStoreOp::STORE);
                    attachment = if let Some(c) = t.clear_color {
                        attachment.load_op(vk::AttachmentLoadOp::CLEAR).clear_value(
                            vk::ClearValue {
                                color: vk::ClearColorValue { float32: c },
                            },
                        )
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
            self.device
                .cmd_begin_rendering(command_buffer, &rendering_info);

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
        self.record_bind_and_draw_into_the_command_buffer(
            command_buffer,
            frame_index,
            offscreen_draw,
        )?;

        unsafe {
            self.device.cmd_end_rendering(command_buffer);

            self.device
                .end_command_buffer(command_buffer)
                .map_err(|e| {
                    Error::GpuError(format!(
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
                    Error::GpuError(format!(
                        "Graphics kernel '{}': post-submit wait failed: {e}",
                        self.label
                    ))
                })?;
        }
        Ok(())
    }

    fn record_bind_and_draw_into_the_command_buffer(
        &self,
        command_buffer: vk::CommandBuffer,
        frame_index: u32,
        draw: DrawKind,
    ) -> Result<()> {
        if frame_index >= self.descriptor_sets_in_flight {
            return Err(Error::GpuError(format!(
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
                return Err(Error::GpuError(format!(
                    "Graphics kernel '{}': binding {} ({:?}) not set before draw",
                    self.label, spec.binding, spec.kind
                )));
            }
        }
        if self.push_constant_size > 0 && pending.push_constants.is_empty() {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{}': push constants not set before draw",
                self.label
            )));
        }
        if let VertexInputState::Buffers { bindings, .. } = &self.pipeline_state.vertex_input {
            for vb in bindings {
                if !pending.vertex_buffers.contains_key(&vb.binding) {
                    return Err(Error::GpuError(format!(
                        "Graphics kernel '{}': vertex buffer at binding {} not set before draw",
                        self.label, vb.binding
                    )));
                }
            }
        }
        if matches!(draw, DrawKind::DrawIndexed(_)) && pending.index_buffer.is_none() {
            return Err(Error::GpuError(format!(
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
                    Error::GpuError(format!(
                        "Graphics kernel '{}': pipeline has dynamic viewport but DrawCall has none",
                        self.label
                    ))
                })?;
                let scissor = scissor_opt.ok_or_else(|| {
                    Error::GpuError(format!(
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
            if let VertexInputState::Buffers { bindings, .. } = &self.pipeline_state.vertex_input {
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
                self.device.cmd_bind_index_buffer(
                    command_buffer,
                    ib.buffer,
                    ib.offset,
                    ib.index_type,
                );
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
                return Err(Error::GpuError(format!(
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

    fn flush_descriptor_writes(&self, frame_index: u32, pending: &PendingState) -> Result<()> {
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
                    return Err(Error::GpuError(format!(
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
