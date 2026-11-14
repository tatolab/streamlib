// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Host-side texture readback: GPU image → HOST_VISIBLE staging buffer.
//!
//! Engine precedent: Unreal `FRHIGPUTextureReadback`, bgfx
//! `bgfx::readTexture`, WebGPU `copyTextureToBuffer + mapAsync`,
//! Granite `copy_image_to_buffer + vkSemaphoreWaitKHR`. Caller creates
//! a handle bound to a fixed format/extent; per submit, the handle
//! transitions the image into `TRANSFER_SRC_OPTIMAL`, copies into a
//! reusable persistent-mapped staging buffer, transitions back, and
//! signals a timeline semaphore. Tickets are timeline counter values.
//!
//! Single-in-flight per handle (the staging buffer + command buffer are
//! one-each, mirroring [`super::VulkanComputeKernel`]). For parallel
//! readbacks, hold N handles.
//!
//! All submits ride [`super::HostVulkanDevice::submit_to_queue`] — same
//! per-queue mutex everything else uses.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use vma::Alloc as _;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::rhi::{
    ReadbackTicket, Texture, TextureFormat, TextureReadbackDescriptor, TextureReadbackError,
    TextureSourceLayout,
};
use crate::core::{Error, Result};
use crate::host_rhi::HostTextureExt;

use super::HostVulkanDevice;

/// Process-wide handle-id counter. Used to tag every readback handle so
/// foreign tickets are caught at try_read / wait_and_read.
static HANDLE_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Host-side texture readback primitive. Bound to a fixed format/extent
/// at construction; the staging buffer + command resources + timeline
/// semaphore are allocated once and reused across every submit.
pub struct VulkanTextureReadback {
    label: String,
    handle_id: u64,
    vulkan_device: Arc<HostVulkanDevice>,
    device: vulkanalia::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    format: TextureFormat,
    width: u32,
    height: u32,
    /// Total staging-buffer size in bytes (`width * height * bpp`).
    bytes: u64,
    /// VMA staging buffer (HOST_VISIBLE | HOST_COHERENT, persistent-mapped).
    staging_buffer: vk::Buffer,
    staging_allocation: vma::Allocation,
    mapped_ptr: *mut u8,
    /// Owned command pool + command buffer. Reset per submit.
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    /// Owned timeline semaphore. Each submit signals at a higher counter
    /// value; tickets are those counter values. `next_counter` is the
    /// value the next submit will signal at.
    timeline: vk::Semaphore,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    /// Counter value the next submit will signal at. Monotonic, never
    /// decreases.
    next_counter: u64,
    /// `Some(counter)` if a submit is in flight at that counter; `None`
    /// when idle.
    pending: Option<u64>,
}

impl VulkanTextureReadback {
    /// Create a new readback handle bound to a fixed format/extent.
    ///
    /// Allocates the staging buffer, command pool/buffer, and timeline
    /// semaphore up-front. On any error every previously-allocated
    /// resource is torn down before returning so the construction path
    /// never leaks.
    #[tracing::instrument(
        skip_all,
        fields(
            rhi_op = "create_texture_readback",
            label = descriptor.label,
            format = ?descriptor.format,
            width = descriptor.width,
            height = descriptor.height,
            bytes = descriptor.staging_size(),
        )
    )]
    pub fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &TextureReadbackDescriptor<'_>,
    ) -> std::result::Result<Self, TextureReadbackError> {
        let label = descriptor.label.to_string();
        let bytes = descriptor.staging_size();
        let queue = vulkan_device.queue();
        let queue_family_index = vulkan_device.queue_family_index();
        let device = vulkan_device.device().clone();

        // ---- Staging buffer (VMA default allocator, HOST_VISIBLE | HOST_COHERENT, persistent-mapped) ----
        // Non-export readback — no DMA-BUF, no custom pool. The VMA
        // default allocator is the right fit; the export pool isolation
        // pattern in `vma-export-pools.md` is for exportable
        // allocations only.
        let buffer_info = vk::BufferCreateInfo::builder()
            .size(bytes)
            .usage(vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let alloc_opts = vma::AllocationOptions {
            // HOST_ACCESS_RANDOM: reading. MAPPED: persistent map.
            flags: vma::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vma::AllocationCreateFlags::MAPPED,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };

        let allocator = vulkan_device.allocator();
        let (staging_buffer, staging_allocation) = unsafe {
            allocator.create_buffer(buffer_info, &alloc_opts)
        }
        .map_err(|e| TextureReadbackError::StagingBufferAlloc {
            label: label.clone(),
            size: bytes,
            cause: e.to_string(),
        })?;

        let alloc_info = allocator.get_allocation_info(staging_allocation);
        let mapped_ptr = alloc_info.pMappedData.cast::<u8>();
        if mapped_ptr.is_null() {
            unsafe { allocator.destroy_buffer(staging_buffer, staging_allocation) };
            return Err(TextureReadbackError::StagingBufferAlloc {
                label,
                size: bytes,
                cause: "VMA returned null mapped pointer despite MAPPED flag".into(),
            });
        }

        // ---- Command pool + buffer (RESET_COMMAND_BUFFER so we can re-record per submit) ----
        let command_pool = match unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(queue_family_index)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .build(),
                None,
            )
        } {
            Ok(p) => p,
            Err(e) => {
                unsafe { allocator.destroy_buffer(staging_buffer, staging_allocation) };
                return Err(TextureReadbackError::CommandResources {
                    label,
                    what: "command pool",
                    cause: e.to_string(),
                });
            }
        };

        let command_buffer = match unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1)
                    .build(),
            )
        } {
            Ok(bufs) => bufs[0],
            Err(e) => {
                unsafe {
                    device.destroy_command_pool(command_pool, None);
                    allocator.destroy_buffer(staging_buffer, staging_allocation);
                }
                return Err(TextureReadbackError::CommandResources {
                    label,
                    what: "command buffer",
                    cause: e.to_string(),
                });
            }
        };

        // ---- Timeline semaphore (counter starts at 0; first submit signals at 1) ----
        let mut type_info = vk::SemaphoreTypeCreateInfo::builder()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0)
            .build();
        let sem_info = vk::SemaphoreCreateInfo::builder()
            .push_next(&mut type_info)
            .build();
        let timeline = match unsafe { device.create_semaphore(&sem_info, None) } {
            Ok(s) => s,
            Err(e) => {
                unsafe {
                    device.destroy_command_pool(command_pool, None);
                    allocator.destroy_buffer(staging_buffer, staging_allocation);
                }
                return Err(TextureReadbackError::CommandResources {
                    label,
                    what: "timeline semaphore",
                    cause: e.to_string(),
                });
            }
        };

        let handle_id = HANDLE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);

        Ok(Self {
            label,
            handle_id,
            vulkan_device: Arc::clone(vulkan_device),
            device,
            queue,
            queue_family_index,
            format: descriptor.format,
            width: descriptor.width,
            height: descriptor.height,
            bytes,
            staging_buffer,
            staging_allocation,
            mapped_ptr,
            command_pool,
            command_buffer,
            timeline,
            state: Mutex::new(State {
                next_counter: 0,
                pending: None,
            }),
        })
    }

    /// Submit a copy of `texture` into the handle's staging buffer.
    ///
    /// `source_layout` is the layout the texture is currently in; the
    /// readback transitions it to `TRANSFER_SRC_OPTIMAL` for the copy
    /// and back to `source_layout` afterward. Single-in-flight: a second
    /// submit before the prior ticket is waited returns
    /// [`TextureReadbackError::InFlight`].
    #[tracing::instrument(
        skip_all,
        fields(
            rhi_op = "texture_readback_submit",
            label = self.label.as_str(),
            handle_id = self.handle_id,
            source_layout = ?source_layout,
        )
    )]
    pub fn submit(
        &self,
        texture: &Texture,
        source_layout: TextureSourceLayout,
    ) -> std::result::Result<ReadbackTicket, TextureReadbackError> {
        // ---- Validate descriptor match ----
        if texture.format() != self.format
            || texture.width() != self.width
            || texture.height() != self.height
        {
            return Err(TextureReadbackError::DescriptorMismatch {
                label: self.label.clone(),
                expected_format: self.format,
                expected_width: self.width,
                expected_height: self.height,
                actual_format: texture.format(),
                actual_width: texture.width(),
                actual_height: texture.height(),
            });
        }

        // Cdylib-safe: `Texture::vulkan_inner()` reaches `host_inner()` which
        // panics under `host_callbacks().is_some()`. Route through the
        // `host_vulkan_texture_arc` FullAccess slot so cdylib callers don't
        // trip the panic guard.
        let host_texture =
            texture
                .host_vulkan_texture_arc()
                .map_err(|e| TextureReadbackError::Submit {
                    label: self.label.clone(),
                    what: "host_vulkan_texture_arc",
                    cause: e.to_string(),
                })?;
        let image = host_texture.image().ok_or_else(|| {
            TextureReadbackError::TextureMissingVulkanImage {
                label: self.label.clone(),
            }
        })?;

        // ---- Single-in-flight check + counter allocation ----
        let counter = {
            let mut state = self.state.lock();
            if let Some(pending) = state.pending {
                return Err(TextureReadbackError::InFlight {
                    label: self.label.clone(),
                    pending,
                });
            }
            state.next_counter += 1;
            let c = state.next_counter;
            state.pending = Some(c);
            c
        };

        // From this point on, any failure must clear `pending` so the
        // handle is recoverable.
        let result = self.record_and_submit(image, source_layout, counter);
        if result.is_err() {
            self.state.lock().pending = None;
        }
        result.map(|_| ReadbackTicket {
            handle_id: self.handle_id,
            counter,
        })
    }

    /// Non-blocking check + read. Returns `Ok(Some(bytes))` if the GPU
    /// copy has completed, `Ok(None)` if still in flight, or `Err` for
    /// foreign / stale tickets and driver errors.
    ///
    /// On `Some`, the handle is reset to idle — the next `submit()` may
    /// overwrite the staging buffer. The returned slice borrows the
    /// handle's mapped memory; the borrow's lifetime ties to `&self`.
    #[tracing::instrument(
        skip_all,
        fields(
            rhi_op = "texture_readback_try_read",
            label = self.label.as_str(),
            handle_id = self.handle_id,
            ticket = ticket.counter,
        )
    )]
    pub fn try_read(
        &self,
        ticket: ReadbackTicket,
    ) -> std::result::Result<Option<&[u8]>, TextureReadbackError> {
        self.validate_ticket(ticket)?;
        let current =
            unsafe { self.device.get_semaphore_counter_value(self.timeline) }.map_err(|e| {
                TextureReadbackError::Submit {
                    label: self.label.clone(),
                    what: "vkGetSemaphoreCounterValue",
                    cause: e.to_string(),
                }
            })?;
        if current < ticket.counter {
            return Ok(None);
        }
        self.state.lock().pending = None;
        Ok(Some(self.mapped_slice()))
    }

    /// Block until the GPU copy completes, then return the staging
    /// buffer contents. Pass `u64::MAX` as `timeout_ns` for "no timeout".
    ///
    /// On success the handle is reset to idle. The returned slice
    /// borrows the handle's mapped memory; the borrow's lifetime ties
    /// to `&self`.
    #[tracing::instrument(
        skip_all,
        fields(
            rhi_op = "texture_readback_wait_and_read",
            label = self.label.as_str(),
            handle_id = self.handle_id,
            ticket = ticket.counter,
            timeout_ns,
        )
    )]
    pub fn wait_and_read(
        &self,
        ticket: ReadbackTicket,
        timeout_ns: u64,
    ) -> std::result::Result<&[u8], TextureReadbackError> {
        self.validate_ticket(ticket)?;
        let semaphores = [self.timeline];
        let values = [ticket.counter];
        let info = vk::SemaphoreWaitInfo::builder()
            .flags(vk::SemaphoreWaitFlags::empty())
            .semaphores(&semaphores)
            .values(&values)
            .build();
        let outcome = unsafe { self.device.wait_semaphores(&info, timeout_ns) }.map_err(|e| {
            TextureReadbackError::Submit {
                label: self.label.clone(),
                what: "vkWaitSemaphores",
                cause: e.to_string(),
            }
        })?;
        // Vulkan reports timeout as `VK_TIMEOUT` (a positive success
        // code), not as an error. vulkanalia returns it via Ok().
        if outcome == vk::SuccessCode::TIMEOUT {
            return Err(TextureReadbackError::WaitTimeout {
                label: self.label.clone(),
                timeout_ns,
            });
        }
        self.state.lock().pending = None;
        Ok(self.mapped_slice())
    }

    /// Borrow the staging buffer through a closure — the closure runs
    /// after `wait_semaphores` returns, then the handle resets to idle.
    /// Useful when the bytes are streamed directly into ffmpeg / a PNG
    /// encoder / etc. without an intermediate `Vec<u8>`.
    pub fn wait_and_read_with<R>(
        &self,
        ticket: ReadbackTicket,
        timeout_ns: u64,
        f: impl FnOnce(&[u8]) -> R,
    ) -> std::result::Result<R, TextureReadbackError> {
        let bytes = self.wait_and_read(ticket, timeout_ns)?;
        Ok(f(bytes))
    }

    /// Format the handle was constructed with.
    pub fn format(&self) -> TextureFormat {
        self.format
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Total staging-buffer size in bytes (`width * height * bpp`).
    pub fn staging_size(&self) -> u64 {
        self.bytes
    }

    /// Process-unique handle id.
    pub fn handle_id(&self) -> u64 {
        self.handle_id
    }

    // ---- internals ----------------------------------------------------------

    fn validate_ticket(
        &self,
        ticket: ReadbackTicket,
    ) -> std::result::Result<(), TextureReadbackError> {
        if ticket.handle_id != self.handle_id {
            return Err(TextureReadbackError::ForeignTicket {
                label: self.label.clone(),
                handle_id: self.handle_id,
                ticket_handle_id: ticket.handle_id,
            });
        }
        let state = self.state.lock();
        match state.pending {
            None => Err(TextureReadbackError::NoSubmission {
                label: self.label.clone(),
            }),
            Some(pending) if pending != ticket.counter => Err(TextureReadbackError::StaleTicket {
                label: self.label.clone(),
                ticket: ticket.counter,
                pending,
            }),
            Some(_) => Ok(()),
        }
    }

    fn mapped_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.mapped_ptr, self.bytes as usize) }
    }

    fn record_and_submit(
        &self,
        image: vk::Image,
        source_layout: TextureSourceLayout,
        counter: u64,
    ) -> std::result::Result<(), TextureReadbackError> {
        let qf = self.queue_family_index;
        let vk_layout = vk_layout_for(source_layout);

        unsafe {
            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| TextureReadbackError::Submit {
                    label: self.label.clone(),
                    what: "vkResetCommandBuffer",
                    cause: e.to_string(),
                })?;

            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build();
            self.device
                .begin_command_buffer(self.command_buffer, &begin_info)
                .map_err(|e| TextureReadbackError::Submit {
                    label: self.label.clone(),
                    what: "vkBeginCommandBuffer",
                    cause: e.to_string(),
                })?;

            // Pre-barrier: source_layout → TRANSFER_SRC_OPTIMAL.
            // ALL_COMMANDS / MEMORY_WRITE in the src masks is the
            // tolerant pattern matching the polyglot examples — covers
            // any stage that may have last written the image.
            let to_src = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
                .old_layout(vk_layout)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_queue_family_index(qf)
                .dst_queue_family_index(qf)
                .image(image)
                .subresource_range(
                    vk::ImageSubresourceRange::builder()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1)
                        .build(),
                )
                .build();
            let pre_barriers = [to_src];
            let pre_dep = vk::DependencyInfo::builder()
                .image_memory_barriers(&pre_barriers)
                .build();
            self.device
                .cmd_pipeline_barrier2(self.command_buffer, &pre_dep);

            // Copy.
            let copy = vk::BufferImageCopy::builder()
                .buffer_offset(0)
                .buffer_row_length(0)
                .buffer_image_height(0)
                .image_subresource(
                    vk::ImageSubresourceLayers::builder()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .layer_count(1)
                        .build(),
                )
                .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .image_extent(vk::Extent3D {
                    width: self.width,
                    height: self.height,
                    depth: 1,
                })
                .build();
            let regions = [copy];
            self.device.cmd_copy_image_to_buffer(
                self.command_buffer,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.staging_buffer,
                &regions,
            );

            // Post-barrier: TRANSFER_SRC_OPTIMAL → source_layout
            // (restore), plus a buffer barrier so the host reading the
            // staging buffer after wait sees the writes.
            let to_orig = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_READ)
                .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .dst_access_mask(vk::AccessFlags2::MEMORY_READ)
                .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .new_layout(vk_layout)
                .src_queue_family_index(qf)
                .dst_queue_family_index(qf)
                .image(image)
                .subresource_range(
                    vk::ImageSubresourceRange::builder()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1)
                        .build(),
                )
                .build();
            let buffer_to_host = vk::BufferMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ)
                .src_queue_family_index(qf)
                .dst_queue_family_index(qf)
                .buffer(self.staging_buffer)
                .offset(0)
                .size(self.bytes)
                .build();
            let post_image = [to_orig];
            let post_buffer = [buffer_to_host];
            let post_dep = vk::DependencyInfo::builder()
                .image_memory_barriers(&post_image)
                .buffer_memory_barriers(&post_buffer)
                .build();
            self.device
                .cmd_pipeline_barrier2(self.command_buffer, &post_dep);

            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| TextureReadbackError::Submit {
                    label: self.label.clone(),
                    what: "vkEndCommandBuffer",
                    cause: e.to_string(),
                })?;

            // Signal the timeline at `counter`.
            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(self.command_buffer)
                .build();
            let sig_info = vk::SemaphoreSubmitInfo::builder()
                .semaphore(self.timeline)
                .value(counter)
                .stage_mask(vk::PipelineStageFlags2::COPY)
                .build();
            let cmd_infos = [cmd_info];
            let sig_infos = [sig_info];
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .signal_semaphore_infos(&sig_infos)
                .build();

            self.vulkan_device
                .submit_to_queue(self.queue, &[submit], vk::Fence::null())
                .map_err(|e| TextureReadbackError::Submit {
                    label: self.label.clone(),
                    what: "queue submit",
                    cause: e.to_string(),
                })?;
        }

        Ok(())
    }
}

fn vk_layout_for(layout: TextureSourceLayout) -> vk::ImageLayout {
    match layout {
        TextureSourceLayout::General => vk::ImageLayout::GENERAL,
        TextureSourceLayout::ColorAttachment => vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        TextureSourceLayout::ShaderReadOnly => vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
    }
}

impl Drop for VulkanTextureReadback {
    fn drop(&mut self) {
        unsafe {
            // Wait only for THIS handle's pending submission rather
            // than calling `device_wait_idle()` — the doc explicitly
            // recommends holding N handles for parallel readback, so a
            // device-wide stall on every handle drop would serialize
            // unrelated work. Timeline starts at 0; if `next_counter`
            // is still 0, no submit has ever happened and the wait
            // returns immediately. If a submit is in flight, waiting
            // on the value that submit signals at is sufficient.
            let target = self.state.lock().next_counter;
            if target > 0 {
                let semaphores = [self.timeline];
                let values = [target];
                let info = vk::SemaphoreWaitInfo::builder()
                    .flags(vk::SemaphoreWaitFlags::empty())
                    .semaphores(&semaphores)
                    .values(&values)
                    .build();
                let _ = self.device.wait_semaphores(&info, u64::MAX);
            }
            self.device.destroy_semaphore(self.timeline, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.vulkan_device
                .allocator()
                .destroy_buffer(self.staging_buffer, self.staging_allocation);
        }
    }
}

// Vulkan handles in this struct are protected by the handle's owned
// timeline semaphore: only one submit is in-flight at a time, and the
// state mutex serializes counter allocation across threads.
unsafe impl Send for VulkanTextureReadback {}
unsafe impl Sync for VulkanTextureReadback {}

impl std::fmt::Debug for VulkanTextureReadback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanTextureReadback")
            .field("label", &self.label)
            .field("handle_id", &self.handle_id)
            .field("format", &self.format)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl From<TextureReadbackError> for Error {
    fn from(e: TextureReadbackError) -> Self {
        Error::GpuError(e.to_string())
    }
}

/// Convenience for callers that already work in `Result<T, Error>`.
impl VulkanTextureReadback {
    pub fn new_into_stream_error(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &TextureReadbackDescriptor<'_>,
    ) -> Result<Self> {
        Self::new(vulkan_device, descriptor).map_err(Error::from)
    }
}

#[cfg(test)]
mod tests;
