// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;
use vma::Alloc as _;

use crate::core::rhi::{
    ReadbackTicket, StreamTexture, TextureFormat, TextureReadbackDescriptor, TextureReadbackError,
    TextureSourceLayout,
};
use crate::core::{Result, StreamError};

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
        let (staging_buffer, staging_allocation) =
            unsafe { allocator.create_buffer(buffer_info, &alloc_opts) }.map_err(|e| {
                TextureReadbackError::StagingBufferAlloc {
                    label: label.clone(),
                    size: bytes,
                    cause: e.to_string(),
                }
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
        texture: &StreamTexture,
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

        let image = texture.vulkan_inner().image().ok_or_else(|| {
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
        let current = unsafe { self.device.get_semaphore_counter_value(self.timeline) }
            .map_err(|e| TextureReadbackError::Submit {
                label: self.label.clone(),
                what: "vkGetSemaphoreCounterValue",
                cause: e.to_string(),
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
            self.device.cmd_pipeline_barrier2(self.command_buffer, &pre_dep);

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
            self.device.cmd_pipeline_barrier2(self.command_buffer, &post_dep);

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
            // Wait for any in-flight submission so we don't tear down
            // resources the GPU still holds.
            let _ = self.device.device_wait_idle();
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

impl From<TextureReadbackError> for StreamError {
    fn from(e: TextureReadbackError) -> Self {
        StreamError::GpuError(e.to_string())
    }
}

/// Convenience for callers that already work in `Result<T, StreamError>`.
impl VulkanTextureReadback {
    pub fn new_into_stream_error(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &TextureReadbackDescriptor<'_>,
    ) -> Result<Self> {
        Self::new(vulkan_device, descriptor).map_err(StreamError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::TextureDescriptor;

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(d) => Some(Arc::new(d)),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                None
            }
        }
    }

    /// Allocate a host-side BGRA8 texture with TRANSFER_SRC + STORAGE
    /// usage and fill plane 0 with a deterministic pattern via a
    /// pre-recorded copy from a HOST_VISIBLE staging buffer.
    fn make_filled_texture(
        device: &Arc<HostVulkanDevice>,
        width: u32,
        height: u32,
        format: TextureFormat,
        pattern: impl Fn(u32, u32) -> [u8; 4],
    ) -> StreamTexture {
        use crate::core::rhi::PixelFormat;
        use crate::vulkan::rhi::HostVulkanPixelBuffer;

        let bpp = format.bytes_per_pixel();
        // Allocate staging via the export-capable pool — fine for fill:
        // we only need HOST_VISIBLE + TRANSFER_SRC.
        let pix_format = match format {
            TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb => PixelFormat::Bgra32,
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => PixelFormat::Bgra32,
            other => panic!("test fixture only supports 8-bit RGBA/BGRA, got {other:?}"),
        };
        let staging =
            HostVulkanPixelBuffer::new(device, width, height, bpp, pix_format).expect("staging");
        unsafe {
            let mut p = staging.mapped_ptr();
            for y in 0..height {
                for x in 0..width {
                    let px = pattern(x, y);
                    std::ptr::copy_nonoverlapping(px.as_ptr(), p, 4);
                    p = p.add(4);
                }
            }
        }

        // Allocate the destination texture via the device's RHI helper —
        // a `STORAGE_BINDING | COPY_SRC | COPY_DST` 2D image. We can't
        // use `acquire_render_target_dma_buf_image` here because tests
        // don't have a `GpuContext`; we go straight to `HostVulkanTexture`.
        let desc = TextureDescriptor {
            width,
            height,
            format,
            usage: crate::core::rhi::TextureUsages::COPY_SRC
                | crate::core::rhi::TextureUsages::COPY_DST
                | crate::core::rhi::TextureUsages::STORAGE_BINDING,
            label: Some("readback-test-texture"),
        };
        let host_tex = crate::vulkan::rhi::HostVulkanTexture::new(device, &desc).expect("texture");
        let texture = StreamTexture {
            inner: Arc::new(host_tex),
        };

        // Record + submit a one-shot copy from staging → texture.
        let dev = device.device();
        let queue = device.queue();
        let qf = device.queue_family_index();
        let pool = unsafe {
            dev.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(qf)
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                    .build(),
                None,
            )
        }
        .expect("pool");
        let cmd = unsafe {
            dev.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1)
                    .build(),
            )
        }
        .expect("cmd")[0];
        unsafe {
            dev.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                    .build(),
            )
            .expect("begin");

            // UNDEFINED → TRANSFER_DST_OPTIMAL.
            let to_dst = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .src_access_mask(vk::AccessFlags2::empty())
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(qf)
                .dst_queue_family_index(qf)
                .image(texture.vulkan_inner().image().expect("vk image"))
                .subresource_range(
                    vk::ImageSubresourceRange::builder()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1)
                        .build(),
                )
                .build();
            let bs = [to_dst];
            let dep = vk::DependencyInfo::builder().image_memory_barriers(&bs).build();
            dev.cmd_pipeline_barrier2(cmd, &dep);

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
                .image_extent(vk::Extent3D { width, height, depth: 1 })
                .build();
            let regions = [copy];
            dev.cmd_copy_buffer_to_image(
                cmd,
                staging.buffer(),
                texture.vulkan_inner().image().expect("vk image"),
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &regions,
            );

            // TRANSFER_DST_OPTIMAL → GENERAL.
            let to_general = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .dst_access_mask(vk::AccessFlags2::MEMORY_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(qf)
                .dst_queue_family_index(qf)
                .image(texture.vulkan_inner().image().expect("vk image"))
                .subresource_range(
                    vk::ImageSubresourceRange::builder()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1)
                        .build(),
                )
                .build();
            let bs2 = [to_general];
            let dep2 = vk::DependencyInfo::builder().image_memory_barriers(&bs2).build();
            dev.cmd_pipeline_barrier2(cmd, &dep2);

            dev.end_command_buffer(cmd).expect("end");
            let cmd_infos = [vk::CommandBufferSubmitInfo::builder().command_buffer(cmd).build()];
            let submits = [vk::SubmitInfo2::builder().command_buffer_infos(&cmd_infos).build()];
            device
                .submit_to_queue(queue, &submits, vk::Fence::null())
                .expect("submit fill");
            dev.queue_wait_idle(queue).expect("wait idle");
            dev.destroy_command_pool(pool, None);
        }

        texture
    }

    /// Positive: round-trip a known pattern through the readback
    /// primitive on a 32x32 texture.
    #[test]
    fn submit_then_wait_returns_expected_bytes() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let width = 32u32;
        let height = 32u32;
        let pattern = |x: u32, y: u32| {
            [
                ((x.wrapping_mul(7)) & 0xFF) as u8,
                ((y.wrapping_mul(11)) & 0xFF) as u8,
                (((x ^ y).wrapping_mul(13)) & 0xFF) as u8,
                0xFF,
            ]
        };
        let texture =
            make_filled_texture(&device, width, height, TextureFormat::Bgra8Unorm, pattern);

        let readback = VulkanTextureReadback::new(
            &device,
            &TextureReadbackDescriptor {
                label: "rt-positive",
                format: TextureFormat::Bgra8Unorm,
                width,
                height,
            },
        )
        .expect("readback");

        let ticket = readback
            .submit(&texture, TextureSourceLayout::General)
            .expect("submit");
        let bytes = readback.wait_and_read(ticket, u64::MAX).expect("wait");
        for y in 0..height {
            for x in 0..width {
                let off = ((y * width + x) * 4) as usize;
                assert_eq!(
                    &bytes[off..off + 4],
                    &pattern(x, y),
                    "mismatch at ({x},{y})"
                );
            }
        }
    }

    /// Positive: multiple submits sequentially on a single handle. After
    /// each wait, the next submit must succeed and the bytes reflect
    /// the (re-filled) texture.
    #[test]
    fn multiple_sequential_submits_work() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let width = 16u32;
        let height = 16u32;
        let readback = VulkanTextureReadback::new(
            &device,
            &TextureReadbackDescriptor {
                label: "rt-sequential",
                format: TextureFormat::Bgra8Unorm,
                width,
                height,
            },
        )
        .expect("readback");

        for run in 0..3u32 {
            let pattern = move |x: u32, y: u32| {
                [
                    ((x + run * 5) & 0xFF) as u8,
                    ((y + run * 7) & 0xFF) as u8,
                    ((run * 11) & 0xFF) as u8,
                    0xFF,
                ]
            };
            let texture =
                make_filled_texture(&device, width, height, TextureFormat::Bgra8Unorm, pattern);
            let ticket = readback.submit(&texture, TextureSourceLayout::General).expect("submit");
            let bytes = readback.wait_and_read(ticket, u64::MAX).expect("wait");
            for y in 0..height {
                for x in 0..width {
                    let off = ((y * width + x) * 4) as usize;
                    assert_eq!(
                        &bytes[off..off + 4],
                        &pattern(x, y),
                        "run {run}: mismatch at ({x},{y})"
                    );
                }
            }
        }
    }

    /// Negative: descriptor / texture mismatch must error at submit time.
    #[test]
    fn rejects_descriptor_mismatch() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let texture = make_filled_texture(
            &device,
            32,
            32,
            TextureFormat::Bgra8Unorm,
            |_, _| [0, 0, 0, 0xFF],
        );

        // Wrong width.
        let readback = VulkanTextureReadback::new(
            &device,
            &TextureReadbackDescriptor {
                label: "rt-wrong-width",
                format: TextureFormat::Bgra8Unorm,
                width: 64,
                height: 32,
            },
        )
        .expect("readback");
        let err = readback
            .submit(&texture, TextureSourceLayout::General)
            .err()
            .expect("expected mismatch");
        assert!(matches!(err, TextureReadbackError::DescriptorMismatch { .. }));

        // Wrong format.
        let readback = VulkanTextureReadback::new(
            &device,
            &TextureReadbackDescriptor {
                label: "rt-wrong-format",
                format: TextureFormat::Rgba16Float,
                width: 32,
                height: 32,
            },
        )
        .expect("readback");
        let err = readback
            .submit(&texture, TextureSourceLayout::General)
            .err()
            .expect("expected mismatch");
        assert!(matches!(err, TextureReadbackError::DescriptorMismatch { .. }));
    }

    /// Negative: a second submit before the first ticket is waited
    /// returns InFlight.
    #[test]
    fn second_submit_before_wait_errors_in_flight() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let texture = make_filled_texture(
            &device,
            32,
            32,
            TextureFormat::Bgra8Unorm,
            |_, _| [1, 2, 3, 4],
        );
        let readback = VulkanTextureReadback::new(
            &device,
            &TextureReadbackDescriptor {
                label: "rt-in-flight",
                format: TextureFormat::Bgra8Unorm,
                width: 32,
                height: 32,
            },
        )
        .expect("readback");
        let ticket = readback.submit(&texture, TextureSourceLayout::General).expect("first");
        let err = readback
            .submit(&texture, TextureSourceLayout::General)
            .err()
            .expect("expected in-flight");
        assert!(matches!(err, TextureReadbackError::InFlight { .. }));
        // Drain so Drop doesn't have to wait on a half-recorded submit.
        let _ = readback.wait_and_read(ticket, u64::MAX).expect("drain");
    }

    /// Negative: a ticket from one handle is rejected by another.
    #[test]
    fn foreign_ticket_rejected() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let texture = make_filled_texture(
            &device,
            16,
            16,
            TextureFormat::Bgra8Unorm,
            |_, _| [0, 0, 0, 0xFF],
        );
        let rb1 = VulkanTextureReadback::new(
            &device,
            &TextureReadbackDescriptor {
                label: "rt-foreign-1",
                format: TextureFormat::Bgra8Unorm,
                width: 16,
                height: 16,
            },
        )
        .expect("rb1");
        let rb2 = VulkanTextureReadback::new(
            &device,
            &TextureReadbackDescriptor {
                label: "rt-foreign-2",
                format: TextureFormat::Bgra8Unorm,
                width: 16,
                height: 16,
            },
        )
        .expect("rb2");
        let ticket = rb1.submit(&texture, TextureSourceLayout::General).expect("submit");
        // rb2 doesn't own this ticket — should reject.
        let err = rb2.try_read(ticket).err().expect("expected foreign");
        assert!(matches!(err, TextureReadbackError::ForeignTicket { .. }));
        let _ = rb1.wait_and_read(ticket, u64::MAX).expect("drain");
    }

    /// Negative: try_read with no in-flight submission errors NoSubmission.
    #[test]
    fn try_read_with_no_submission_errors_no_submission() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let readback = VulkanTextureReadback::new(
            &device,
            &TextureReadbackDescriptor {
                label: "rt-no-sub",
                format: TextureFormat::Bgra8Unorm,
                width: 8,
                height: 8,
            },
        )
        .expect("readback");
        let ticket = ReadbackTicket {
            handle_id: readback.handle_id,
            counter: 1,
        };
        let err = readback.try_read(ticket).err().expect("expected no submission");
        assert!(matches!(err, TextureReadbackError::NoSubmission { .. }));
    }

    /// Multiple readback handles can be in flight concurrently.
    /// Validates the design assertion that parallel readbacks are
    /// achieved by holding N handles, not by parallelism on one.
    #[test]
    fn multiple_handles_can_be_in_flight_concurrently() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let texture = make_filled_texture(
            &device,
            16,
            16,
            TextureFormat::Bgra8Unorm,
            |x, y| [(x as u8), (y as u8), (x as u8) ^ (y as u8), 0xFF],
        );
        let rb1 = VulkanTextureReadback::new(
            &device,
            &TextureReadbackDescriptor {
                label: "rt-parallel-1",
                format: TextureFormat::Bgra8Unorm,
                width: 16,
                height: 16,
            },
        )
        .expect("rb1");
        let rb2 = VulkanTextureReadback::new(
            &device,
            &TextureReadbackDescriptor {
                label: "rt-parallel-2",
                format: TextureFormat::Bgra8Unorm,
                width: 16,
                height: 16,
            },
        )
        .expect("rb2");
        let t1 = rb1.submit(&texture, TextureSourceLayout::General).expect("submit 1");
        let t2 = rb2.submit(&texture, TextureSourceLayout::General).expect("submit 2");
        // Both tickets must wait + read independently.
        let _ = rb1.wait_and_read(t1, u64::MAX).expect("read 1");
        let _ = rb2.wait_and_read(t2, u64::MAX).expect("read 2");
    }

    /// Drop on an idle handle must not panic.
    #[test]
    fn drop_on_idle_handle_no_panic() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let readback = VulkanTextureReadback::new(
            &device,
            &TextureReadbackDescriptor {
                label: "rt-drop-idle",
                format: TextureFormat::Bgra8Unorm,
                width: 8,
                height: 8,
            },
        )
        .expect("readback");
        drop(readback);
    }

    /// Bytes-per-pixel for Bgra8Unorm and Rgba8Unorm: descriptor sizes
    /// match the staging buffer and constructed handles report the
    /// expected dimensions.
    #[test]
    fn descriptor_sizes_and_metadata_round_trip() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let descriptor = TextureReadbackDescriptor {
            label: "rt-meta",
            format: TextureFormat::Bgra8Unorm,
            width: 100,
            height: 50,
        };
        assert_eq!(descriptor.staging_size(), 100 * 50 * 4);
        let readback = VulkanTextureReadback::new(&device, &descriptor).expect("readback");
        assert_eq!(readback.format(), TextureFormat::Bgra8Unorm);
        assert_eq!(readback.width(), 100);
        assert_eq!(readback.height(), 50);
        assert_eq!(readback.staging_size(), 100 * 50 * 4);
        assert!(readback.handle_id() > 0);
    }
}
