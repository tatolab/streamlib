// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The owned-device tier for an embedded GPU renderer: a Vulkan device a
//! module creates on the runtime's GPU, whose raw handles a third-party
//! renderer (Rive, Skia, a game engine) builds its own context on, and onto
//! which node output textures import as render targets.
//!
//! This is the one place raw Vulkan handles leave the RHI. The renderer's own
//! code makes its Vulkan calls on them; the module's Rust names no Vulkan
//! type. Each frame is recorded by the renderer into a command buffer this
//! device begins, and submitted here, so the acquire and release barriers and
//! the surface-share timeline edge around the renderer's work stay the RHI's.

use std::os::fd::{FromRawFd as _, IntoRawFd as _, OwnedFd};
use std::sync::{Arc, Mutex, MutexGuard};

use streamlib_surface_client::{CheckedOutTexture, CheckedOutTextureMemory};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::{
    ConsumerRhiError, ConsumerVulkanDevice, ConsumerVulkanTexture, ConsumerVulkanTimelineSemaphore,
    REQUESTED_VULKAN_INSTANCE_API_VERSION, Result, TextureFormat, VulkanLayout, VulkanTextureLike,
};

/// The layout a rendered frame is published in: what every engine reader of
/// a node output texture samples from.
const LAYOUT_A_RENDERED_FRAME_IS_PUBLISHED_IN: VulkanLayout =
    VulkanLayout::SHADER_READ_ONLY_OPTIMAL;

/// The core `VkPhysicalDeviceFeatures` the device enabled, as `VkBool32`s.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExternalRendererEnabledVulkanFeatures {
    pub independent_blend: u32,
    pub fill_mode_non_solid: u32,
    pub fragment_stores_and_atomics: u32,
    pub shader_clip_distance: u32,
    pub sampler_anisotropy: u32,
    pub depth_bias_clamp: u32,
}

/// The raw Vulkan handles an embedded renderer builds its context on.
///
/// Dispatchable handles and the function pointer are addresses; the
/// non-dispatchable queue family is an index. All stay valid for the life
/// of the [`ConsumerVulkanDeviceForExternalRenderer`] that handed them out.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalRendererRawVulkanHandles {
    /// `VkInstance`.
    pub vk_instance: u64,
    /// `VkPhysicalDevice`.
    pub vk_physical_device: u64,
    /// `VkDevice`.
    pub vk_device: u64,
    /// The graphics `VkQueue` frames are submitted on. The renderer never
    /// submits to it itself.
    pub vk_graphics_queue: u64,
    /// `PFN_vkGetInstanceProcAddr`.
    pub vk_get_instance_proc_addr: u64,
    /// The family [`Self::vk_graphics_queue`] belongs to.
    pub graphics_queue_family_index: u32,
    /// The API version the instance was created with, as `VK_MAKE_API_VERSION`.
    pub vulkan_api_version: u32,
    pub enabled_features: ExternalRendererEnabledVulkanFeatures,
}

/// One frame's recording: the command buffer the renderer records into and
/// the target it draws to, as raw handles.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalRendererFrameRawVulkanHandles {
    /// A begun `VkCommandBuffer` on the graphics queue's family.
    pub vk_command_buffer: u64,
    /// The render target's `VkImage`.
    pub vk_target_image: u64,
    /// A `VkImageView` over the whole target.
    pub vk_target_image_view: u64,
    /// The target's `VkFormat`.
    pub vk_target_format: i32,
    /// The `VkImageLayout` the target is in when the renderer's commands
    /// begin: `COLOR_ATTACHMENT_OPTIMAL`, its prior contents discarded.
    pub vk_target_layout: i32,
    /// The pipeline stages that last touched the target before the
    /// renderer's commands, as `VkPipelineStageFlags`.
    pub vk_target_last_pipeline_stages: u32,
    /// The accesses that last touched the target, as `VkAccessFlags`.
    pub vk_target_last_access_mask: u32,
    /// The `VkImageUsageFlags` the target was created with.
    pub vk_target_usage_flags: u32,
    pub target_width: u32,
    pub target_height: u32,
    /// The number of this frame, counted from one; every earlier frame's GPU
    /// work has retired.
    pub current_frame_number: u64,
    /// The newest frame whose GPU work has retired.
    pub safe_frame_number: u64,
}

/// A node output texture imported onto the external-renderer device as a
/// render target, holding the surface-share timeline edges of its crossing.
pub struct ExternalRendererRenderTarget {
    texture: ConsumerVulkanTexture,
    produce_done_timeline: Option<ConsumerVulkanTimelineSemaphore>,
    _consume_done_timeline: Option<ConsumerVulkanTimelineSemaphore>,
}

impl ExternalRendererRenderTarget {
    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.texture.width()
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.texture.height()
    }

    /// The texture format the target was registered with.
    pub fn format(&self) -> TextureFormat {
        self.texture.format()
    }
}

/// The reusable recording state: one frame is in flight at a time.
struct ExternalRendererFrameRecording {
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    frame_retired_fence: vk::Fence,
    frames_retired: u64,
}

/// A module-owned Vulkan device an embedded renderer draws on — see the
/// module docs.
pub struct ConsumerVulkanDeviceForExternalRenderer {
    frame_recording: Mutex<ExternalRendererFrameRecording>,
    consumer_vulkan_device: Arc<ConsumerVulkanDevice>,
}

impl ConsumerVulkanDeviceForExternalRenderer {
    /// Create the device on the GPU whose `deviceUUID` is
    /// `vulkan_device_uuid_the_runtime_named`.
    pub fn new_on_the_device_the_runtime_named(
        vulkan_device_uuid_the_runtime_named: [u8; 16],
    ) -> Result<Self> {
        let consumer_vulkan_device = Arc::new(
            ConsumerVulkanDevice::new_for_an_external_renderer_on_the_device_the_runtime_named(
                vulkan_device_uuid_the_runtime_named,
            )?,
        );
        let device = consumer_vulkan_device.device();
        let command_pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(consumer_vulkan_device.queue_family_index())
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                None,
            )
        }
        .map_err(|e| ConsumerRhiError::Gpu(format!("external renderer command pool: {e}")))?;
        let command_buffer = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map(|command_buffers| command_buffers[0])
        .map_err(|e| {
            unsafe { device.destroy_command_pool(command_pool, None) };
            ConsumerRhiError::Gpu(format!("external renderer command buffer: {e}"))
        })?;
        let frame_retired_fence =
            unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }.map_err(|e| {
                unsafe { device.destroy_command_pool(command_pool, None) };
                ConsumerRhiError::Gpu(format!("external renderer fence: {e}"))
            })?;
        let enabled_features = consumer_vulkan_device.enabled_physical_device_features();
        tracing::info!(
            device = consumer_vulkan_device.name(),
            graphics_queue_family_index = consumer_vulkan_device.queue_family_index(),
            fragment_stores_and_atomics = enabled_features.fragment_stores_and_atomics != 0,
            independent_blend = enabled_features.independent_blend != 0,
            shader_clip_distance = enabled_features.shader_clip_distance != 0,
            "external-renderer Vulkan device created on the device the runtime named"
        );
        Ok(Self {
            frame_recording: Mutex::new(ExternalRendererFrameRecording {
                command_pool,
                command_buffer,
                frame_retired_fence,
                frames_retired: 0,
            }),
            consumer_vulkan_device,
        })
    }

    /// The device's name, as the driver reports it.
    pub fn name(&self) -> &str {
        self.consumer_vulkan_device.name()
    }

    /// The raw handles an embedded renderer creates its context from.
    pub fn raw_vulkan_handles_for_the_external_renderer(&self) -> ExternalRendererRawVulkanHandles {
        let consumer_vulkan_device = &self.consumer_vulkan_device;
        let features = consumer_vulkan_device.enabled_physical_device_features();
        ExternalRendererRawVulkanHandles {
            vk_instance: consumer_vulkan_device.instance().handle().as_raw() as u64,
            vk_physical_device: consumer_vulkan_device.physical_device().as_raw() as u64,
            vk_device: consumer_vulkan_device.device().handle().as_raw() as u64,
            vk_graphics_queue: consumer_vulkan_device.queue().as_raw() as u64,
            vk_get_instance_proc_addr: consumer_vulkan_device.get_instance_proc_addr() as usize
                as u64,
            graphics_queue_family_index: consumer_vulkan_device.queue_family_index(),
            vulkan_api_version: REQUESTED_VULKAN_INSTANCE_API_VERSION,
            enabled_features: ExternalRendererEnabledVulkanFeatures {
                independent_blend: features.independent_blend,
                fill_mode_non_solid: features.fill_mode_non_solid,
                fragment_stores_and_atomics: features.fragment_stores_and_atomics,
                shader_clip_distance: features.shader_clip_distance,
                sampler_anisotropy: features.sampler_anisotropy,
                depth_bias_clamp: features.depth_bias_clamp,
            },
        }
    }

    /// Import a node output texture checked out over surface-share as a
    /// render target, joining its timeline pair.
    pub fn import_node_output_texture_as_render_target(
        &self,
        surface_id: &str,
        checked_out: CheckedOutTexture,
    ) -> Result<ExternalRendererRenderTarget> {
        let format =
            TextureFormat::from_wire_name(&checked_out.format_wire_name).ok_or_else(|| {
                ConsumerRhiError::Gpu(format!(
                    "texture {surface_id:?} is registered with format {:?}, which the RHI does not \
                 know",
                    checked_out.format_wire_name
                ))
            })?;
        let device = &self.consumer_vulkan_device;
        let texture = match checked_out.memory {
            CheckedOutTextureMemory::DmaBuf {
                memory_fd,
                plane_offsets,
                plane_strides,
                drm_format_modifier,
            } => {
                let raw_memory_fd = memory_fd.into_raw_fd();
                ConsumerVulkanTexture::import_render_target_dma_buf(
                    device,
                    &[raw_memory_fd],
                    &plane_offsets,
                    &plane_strides,
                    drm_format_modifier,
                    checked_out.width,
                    checked_out.height,
                    format,
                    checked_out.allocation_byte_size,
                )
                .inspect_err(|_| {
                    // SAFETY: Vulkan takes the fd only on success.
                    drop(unsafe { OwnedFd::from_raw_fd(raw_memory_fd) });
                })?
            }
            CheckedOutTextureMemory::OpaqueFd {
                memory_fd,
                vk_memory_type_index,
                exporting_device_uuid,
            } => {
                let vk_image_usage_flags = checked_out.vk_image_usage_flags;
                if exporting_device_uuid != device.physical_device_uuid() {
                    return Err(ConsumerRhiError::Gpu(format!(
                        "texture {surface_id:?} was exported by another GPU; an OPAQUE_FD \
                         import onto {} would read the wrong memory",
                        device.name()
                    )));
                }
                if !vk::ImageUsageFlags::from_bits_truncate(vk_image_usage_flags)
                    .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)
                {
                    return Err(ConsumerRhiError::Gpu(format!(
                        "texture {surface_id:?} was created without render_attachment usage, so \
                         a renderer cannot draw to it; acquire it with render_attachment"
                    )));
                }
                let raw_memory_fd = memory_fd.into_raw_fd();
                ConsumerVulkanTexture::from_opaque_fd(
                    device,
                    raw_memory_fd,
                    checked_out.width,
                    checked_out.height,
                    format,
                    vk_image_usage_flags,
                    checked_out.allocation_byte_size,
                    vk_memory_type_index,
                )
                .inspect_err(|_| {
                    // SAFETY: Vulkan takes the fd only on success.
                    drop(unsafe { OwnedFd::from_raw_fd(raw_memory_fd) });
                })?
            }
        };
        Ok(ExternalRendererRenderTarget {
            texture,
            produce_done_timeline: self.import_timeline_edge(
                surface_id,
                "produce_done",
                checked_out.produce_done_timeline_fd,
            )?,
            _consume_done_timeline: self.import_timeline_edge(
                surface_id,
                "consume_done",
                checked_out.consume_done_timeline_fd,
            )?,
        })
    }

    fn import_timeline_edge(
        &self,
        surface_id: &str,
        edge_name: &str,
        edge_fd: Option<OwnedFd>,
    ) -> Result<Option<ConsumerVulkanTimelineSemaphore>> {
        let Some(edge_fd) = edge_fd else {
            return Ok(None);
        };
        let raw_edge_fd = edge_fd.into_raw_fd();
        ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd(
            &self.consumer_vulkan_device,
            raw_edge_fd,
        )
        .map(Some)
        .map_err(|import_failure| {
            // SAFETY: Vulkan takes the fd only on success.
            drop(unsafe { OwnedFd::from_raw_fd(raw_edge_fd) });
            ConsumerRhiError::Gpu(format!(
                "texture {surface_id:?}'s {edge_name} timeline would not import: {import_failure}"
            ))
        })
    }

    /// Begin a frame drawing into `render_target`: the command buffer is
    /// begun with the target acquired into `COLOR_ATTACHMENT_OPTIMAL`, and
    /// every earlier frame has retired.
    pub fn begin_frame_into<'frame>(
        &'frame self,
        render_target: &'frame ExternalRendererRenderTarget,
    ) -> Result<ExternalRendererFrame<'frame>> {
        let recording = self
            .frame_recording
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let consumer_vulkan_device = &self.consumer_vulkan_device;
        let device = consumer_vulkan_device.device();
        unsafe {
            device.reset_command_pool(recording.command_pool, vk::CommandPoolResetFlags::empty())
        }
        .map_err(|e| ConsumerRhiError::Gpu(format!("external renderer command pool reset: {e}")))?;
        unsafe {
            device.begin_command_buffer(
                recording.command_buffer,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .map_err(|e| ConsumerRhiError::Gpu(format!("external renderer frame begin: {e}")))?;

        let source_queue_family_index = if consumer_vulkan_device.supports_qfot_acquire_unmodified()
        {
            vk::QUEUE_FAMILY_EXTERNAL
        } else {
            consumer_vulkan_device.queue_family_index()
        };
        let acquire_discarding_prior_contents = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
            .dst_access_mask(
                vk::AccessFlags2::COLOR_ATTACHMENT_READ | vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            )
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .src_queue_family_index(source_queue_family_index)
            .dst_queue_family_index(consumer_vulkan_device.queue_family_index())
            .image(render_target.texture.image())
            .subresource_range(whole_colour_subresource_range())
            .build();
        let barriers = [acquire_discarding_prior_contents];
        unsafe {
            device.cmd_pipeline_barrier2(
                recording.command_buffer,
                &vk::DependencyInfo::builder().image_memory_barriers(&barriers),
            )
        };

        let raw_vulkan_handles = ExternalRendererFrameRawVulkanHandles {
            vk_command_buffer: recording.command_buffer.as_raw() as u64,
            vk_target_image: render_target.texture.image().as_raw(),
            vk_target_image_view: render_target.texture.image_view()?.as_raw(),
            vk_target_format: render_target.texture.vk_format().as_raw(),
            vk_target_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL.as_raw(),
            vk_target_last_pipeline_stages: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT.bits(),
            vk_target_last_access_mask: (vk::AccessFlags::COLOR_ATTACHMENT_READ
                | vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .bits(),
            vk_target_usage_flags: render_target.texture.vk_image_usage_flags().bits(),
            target_width: render_target.width(),
            target_height: render_target.height(),
            current_frame_number: recording.frames_retired + 1,
            safe_frame_number: recording.frames_retired,
        };
        Ok(ExternalRendererFrame {
            recording,
            device_for_external_renderer: self,
            render_target,
            raw_vulkan_handles,
        })
    }
}

impl Drop for ConsumerVulkanDeviceForExternalRenderer {
    fn drop(&mut self) {
        let recording = self
            .frame_recording
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let device = self.consumer_vulkan_device.device();
        unsafe {
            device.destroy_fence(recording.frame_retired_fence, None);
            device.destroy_command_pool(recording.command_pool, None);
        }
    }
}

/// A frame being recorded into a render target. Dropped without
/// [`Self::submit_and_wait`], it submits nothing and the next frame begins
/// afresh.
pub struct ExternalRendererFrame<'frame> {
    recording: MutexGuard<'frame, ExternalRendererFrameRecording>,
    device_for_external_renderer: &'frame ConsumerVulkanDeviceForExternalRenderer,
    render_target: &'frame ExternalRendererRenderTarget,
    raw_vulkan_handles: ExternalRendererFrameRawVulkanHandles,
}

impl ExternalRendererFrame<'_> {
    /// What the renderer records with.
    pub fn raw_vulkan_handles(&self) -> ExternalRendererFrameRawVulkanHandles {
        self.raw_vulkan_handles
    }

    /// Release the target to the engine from the layout the renderer's
    /// commands left it in, submit, signal the crossing's produce-done edge,
    /// and wait for the frame to retire. Answers the layout to publish over
    /// surface-share.
    pub fn submit_and_wait(
        mut self,
        layout_the_renderer_left_the_target_in: VulkanLayout,
    ) -> Result<VulkanLayout> {
        let consumer_vulkan_device = &self.device_for_external_renderer.consumer_vulkan_device;
        let device = consumer_vulkan_device.device();
        let command_buffer = self.recording.command_buffer;
        let release_to_the_engine = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::NONE)
            .dst_access_mask(vk::AccessFlags2::empty())
            .old_layout(layout_the_renderer_left_the_target_in.as_vk())
            .new_layout(LAYOUT_A_RENDERED_FRAME_IS_PUBLISHED_IN.as_vk())
            .src_queue_family_index(consumer_vulkan_device.queue_family_index())
            .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
            .image(self.render_target.texture.image())
            .subresource_range(whole_colour_subresource_range())
            .build();
        let barriers = [release_to_the_engine];
        unsafe {
            device.cmd_pipeline_barrier2(
                command_buffer,
                &vk::DependencyInfo::builder().image_memory_barriers(&barriers),
            );
            device.end_command_buffer(command_buffer)
        }
        .map_err(|e| ConsumerRhiError::Gpu(format!("external renderer frame end: {e}")))?;

        let produce_done_signals = match &self.render_target.produce_done_timeline {
            Some(produce_done) => vec![
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(produce_done.semaphore())
                    .value(produce_done.current_value()? + 1)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .build(),
            ],
            None => Vec::new(),
        };
        let command_buffer_infos = [vk::CommandBufferSubmitInfo::builder()
            .command_buffer(command_buffer)
            .build()];
        let submits = [vk::SubmitInfo2::builder()
            .command_buffer_infos(&command_buffer_infos)
            .signal_semaphore_infos(&produce_done_signals)
            .build()];
        let frame_retired_fence = self.recording.frame_retired_fence;
        // SAFETY: the command buffer and fence are this device's, and the
        // signalled timeline was imported onto it.
        unsafe {
            consumer_vulkan_device.submit_to_queue(
                consumer_vulkan_device.queue(),
                &submits,
                frame_retired_fence,
            )
        }?;
        unsafe {
            device
                .wait_for_fences(&[frame_retired_fence], true, u64::MAX)
                .and_then(|_| device.reset_fences(&[frame_retired_fence]))
        }
        .map_err(|e| ConsumerRhiError::Gpu(format!("external renderer frame wait: {e}")))?;
        self.recording.frames_retired += 1;
        Ok(LAYOUT_A_RENDERED_FRAME_IS_PUBLISHED_IN)
    }
}

fn whole_colour_subresource_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::*;

    #[test]
    fn the_device_handles_lay_out_as_the_c_struct_a_renderer_reads() {
        assert_eq!(size_of::<ExternalRendererEnabledVulkanFeatures>(), 24);
        assert_eq!(size_of::<ExternalRendererRawVulkanHandles>(), 72);
        assert_eq!(offset_of!(ExternalRendererRawVulkanHandles, vk_instance), 0);
        assert_eq!(
            offset_of!(ExternalRendererRawVulkanHandles, vk_physical_device),
            8
        );
        assert_eq!(offset_of!(ExternalRendererRawVulkanHandles, vk_device), 16);
        assert_eq!(
            offset_of!(ExternalRendererRawVulkanHandles, vk_graphics_queue),
            24
        );
        assert_eq!(
            offset_of!(ExternalRendererRawVulkanHandles, vk_get_instance_proc_addr),
            32
        );
        assert_eq!(
            offset_of!(
                ExternalRendererRawVulkanHandles,
                graphics_queue_family_index
            ),
            40
        );
        assert_eq!(
            offset_of!(ExternalRendererRawVulkanHandles, vulkan_api_version),
            44
        );
        assert_eq!(
            offset_of!(ExternalRendererRawVulkanHandles, enabled_features),
            48
        );
    }

    #[test]
    fn the_frame_handles_lay_out_as_the_c_struct_a_renderer_reads() {
        assert_eq!(size_of::<ExternalRendererFrameRawVulkanHandles>(), 72);
        assert_eq!(
            offset_of!(ExternalRendererFrameRawVulkanHandles, vk_command_buffer),
            0
        );
        assert_eq!(
            offset_of!(ExternalRendererFrameRawVulkanHandles, vk_target_image),
            8
        );
        assert_eq!(
            offset_of!(ExternalRendererFrameRawVulkanHandles, vk_target_image_view),
            16
        );
        assert_eq!(
            offset_of!(ExternalRendererFrameRawVulkanHandles, vk_target_format),
            24
        );
        assert_eq!(
            offset_of!(ExternalRendererFrameRawVulkanHandles, vk_target_layout),
            28
        );
        assert_eq!(
            offset_of!(
                ExternalRendererFrameRawVulkanHandles,
                vk_target_last_pipeline_stages
            ),
            32
        );
        assert_eq!(
            offset_of!(
                ExternalRendererFrameRawVulkanHandles,
                vk_target_last_access_mask
            ),
            36
        );
        assert_eq!(
            offset_of!(ExternalRendererFrameRawVulkanHandles, vk_target_usage_flags),
            40
        );
        assert_eq!(
            offset_of!(ExternalRendererFrameRawVulkanHandles, target_width),
            44
        );
        assert_eq!(
            offset_of!(ExternalRendererFrameRawVulkanHandles, target_height),
            48
        );
        assert_eq!(
            offset_of!(ExternalRendererFrameRawVulkanHandles, current_frame_number),
            56
        );
        assert_eq!(
            offset_of!(ExternalRendererFrameRawVulkanHandles, safe_frame_number),
            64
        );
    }
}
