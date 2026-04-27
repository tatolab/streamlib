// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `CpuReadbackSurfaceAdapter` — host-side `SurfaceAdapter` that returns
//! a CPU byte slice on every acquire.
//!
//! On `acquire_*`:
//!  1. Wait for prior GPU work to drain (timeline semaphore wait).
//!  2. Transition the host's `VkImage` into `TRANSFER_SRC_OPTIMAL`.
//!  3. `vkCmdCopyImageToBuffer` into the per-plane staging buffer(s).
//!     For multi-plane formats (NV12) one region per plane is recorded
//!     with the corresponding `VK_IMAGE_ASPECT_PLANE_{0,1,…}_BIT`.
//!  4. Block on `vkQueueWaitIdle` so the bytes are observable from the
//!     CPU side once the call returns.
//!  5. Hand the customer per-plane `&[u8]` views over the mapped staging
//!     buffers.
//!
//! On WRITE guard `Drop`:
//!  1. `vkCmdCopyBufferToImage` per plane to flush CPU edits back into
//!     the host `VkImage`.
//!  2. Transition the image to `GENERAL` so the next consumer sees a
//!     deterministic layout.
//!  3. Signal the next timeline release-value.
//!
//! READ guard `Drop` simply signals the timeline; nothing is flushed
//! back since the customer can't have mutated the read view.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use streamlib::adapter_support::{VulkanDevice, VulkanPixelBuffer, VulkanTimelineSemaphore};
use streamlib::core::rhi::PixelFormat;
use streamlib_adapter_abi::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceId, WriteGuard,
};
use tracing::instrument;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::state::{HostSurfaceRegistration, PlaneSlot, SurfaceState, VulkanLayout};
use crate::view::{
    CpuReadbackPlaneView, CpuReadbackPlaneViewMut, CpuReadbackReadView, CpuReadbackWriteView,
};

/// Default per-acquire timeline-wait timeout.
const DEFAULT_TIMELINE_WAIT: Duration = Duration::from_secs(5);

/// Explicit GPU→CPU [`SurfaceAdapter`] implementation.
///
/// Construct with [`Self::new`] passing the host's [`VulkanDevice`].
/// Register host-allocated surfaces with [`Self::register_host_surface`];
/// each registration allocates one dedicated [`VulkanPixelBuffer`] per
/// plane (HOST_VISIBLE/HOST_COHERENT linear buffer, sized exactly to the
/// plane's pixel footprint) used as the staging area for image↔buffer
/// copies. Consumers acquire scoped access through the standard
/// [`SurfaceAdapter::acquire_read`] / [`SurfaceAdapter::acquire_write`]
/// API or via the [`crate::CpuReadbackContext`] convenience.
pub struct CpuReadbackSurfaceAdapter {
    device: Arc<VulkanDevice>,
    surfaces: Mutex<HashMap<SurfaceId, SurfaceState>>,
    acquire_timeout: Duration,
}

impl CpuReadbackSurfaceAdapter {
    /// Construct an empty adapter bound to `device`.
    pub fn new(device: Arc<VulkanDevice>) -> Self {
        Self {
            device,
            surfaces: Mutex::new(HashMap::new()),
            acquire_timeout: DEFAULT_TIMELINE_WAIT,
        }
    }

    /// Override the per-acquire timeline-wait timeout. Default 5 s.
    pub fn with_acquire_timeout(mut self, timeout: Duration) -> Self {
        self.acquire_timeout = timeout;
        self
    }

    /// Returns the underlying device.
    pub fn device(&self) -> &Arc<VulkanDevice> {
        &self.device
    }

    /// Register a host-allocated surface with this adapter.
    ///
    /// Allocates one dedicated `VulkanPixelBuffer` per plane (HOST_VISIBLE,
    /// HOST_COHERENT, linear). Plane geometry is derived from
    /// [`HostSurfaceRegistration::format`] via [`SurfaceFormat::plane_count`]
    /// and [`SurfaceFormat::plane_byte_size`].
    #[instrument(level = "debug", skip(self, registration), fields(surface_id = id))]
    pub fn register_host_surface(
        &self,
        id: SurfaceId,
        registration: HostSurfaceRegistration,
    ) -> Result<(), AdapterError> {
        let mut map = self.surfaces.lock();
        if map.contains_key(&id) {
            return Err(AdapterError::SurfaceNotFound { surface_id: id });
        }

        let width = registration.texture.width();
        let height = registration.texture.height();
        let format = registration.format;

        // Validate dimensions are compatible with the format's chroma
        // subsampling. NV12's UV plane is at half-resolution, so the
        // surface must be even-sized to round-trip exactly. Odd sizes
        // would silently lose the trailing column / row.
        if format.plane_count() > 1 && (width % 2 != 0 || height % 2 != 0) {
            return Err(AdapterError::UnsupportedFormat {
                surface_id: id,
                reason: format!(
                    "{format:?} requires even surface dimensions for chroma subsampling, got {width}x{height}"
                ),
            });
        }

        let plane_count = format.plane_count();
        let mut planes = Vec::with_capacity(plane_count as usize);
        for plane_idx in 0..plane_count {
            let pw = format.plane_width(width, plane_idx);
            let ph = format.plane_height(height, plane_idx);
            let pbpp = format.plane_bytes_per_pixel(plane_idx);

            // Allocate a *dedicated* HOST_VISIBLE linear staging buffer per
            // plane. Going through `GpuContext::acquire_pixel_buffer` would
            // draw from the shared (w,h,format) pool and cap at 4 surfaces
            // of identical dimensions — wrong shape for an adapter that
            // needs one buffer per registered surface plane.
            //
            // The `PixelFormat` argument to `VulkanPixelBuffer::new` is
            // opaque metadata only — `bytes_per_pixel` drives the
            // allocation size. We pass `Bgra32` uniformly so the
            // staging buffer's recorded format isn't claiming a specific
            // pixel layout (Y/UV/RGB) the adapter never interprets.
            let staging = Arc::new(
                VulkanPixelBuffer::new(&self.device, pw, ph, pbpp, PixelFormat::Bgra32).map_err(
                    |e| AdapterError::IpcDisconnected {
                        reason: format!(
                            "VulkanPixelBuffer::new for cpu-readback staging plane {plane_idx}: {e}"
                        ),
                    },
                )?,
            );
            planes.push(PlaneSlot {
                staging,
                width: pw,
                height: ph,
                bytes_per_pixel: pbpp,
            });
        }

        map.insert(
            id,
            SurfaceState {
                surface_id: id,
                texture: registration.texture,
                planes,
                timeline: registration.timeline,
                current_layout: VulkanLayout(registration.initial_image_layout),
                read_holders: 0,
                write_held: false,
                current_release_value: 0,
                format,
                width,
                height,
            },
        );
        Ok(())
    }

    /// Drop a registered surface.
    pub fn unregister_host_surface(&self, id: SurfaceId) -> bool {
        self.surfaces.lock().remove(&id).is_some()
    }

    /// Snapshot the registry size — primarily for tests / observability.
    pub fn registered_count(&self) -> usize {
        self.surfaces.lock().len()
    }

    /// Common acquire path: wait timeline, then issue
    /// `vkCmdCopyImageToBuffer` into the per-plane staging buffers.
    /// Returns the snapshot needed to build a view, with state's
    /// `read_holders` / `write_held` already incremented.
    fn try_begin(
        &self,
        surface: &StreamlibSurface,
        write: bool,
    ) -> Result<Option<AcquireSnapshot>, AdapterError> {
        let mut map = self.surfaces.lock();
        let state = map
            .get_mut(&surface.id)
            .ok_or(AdapterError::SurfaceNotFound { surface_id: surface.id })?;

        if state.write_held {
            return Ok(None);
        }
        if write && state.read_holders > 0 {
            return Ok(None);
        }

        let timeline = Arc::clone(&state.timeline);
        let wait_value = state.current_release_value;
        let image = state
            .texture
            .vulkan_inner()
            .image()
            .ok_or(AdapterError::SurfaceNotFound { surface_id: surface.id })?;
        let from = state.current_layout;
        let format = state.format;
        let width = state.width;
        let height = state.height;
        let plane_snaps: Vec<PlaneAcquireSlot> = state
            .planes
            .iter()
            .map(|p| PlaneAcquireSlot {
                buffer: p.staging.buffer(),
                mapped_ptr: p.staging.mapped_ptr(),
                width: p.width,
                height: p.height,
                bytes_per_pixel: p.bytes_per_pixel,
                byte_size: p.byte_size(),
            })
            .collect();

        if write {
            state.write_held = true;
        } else {
            state.read_holders += 1;
        }

        Ok(Some(AcquireSnapshot {
            timeline,
            wait_value,
            image,
            from,
            format,
            width,
            height,
            planes: plane_snaps,
        }))
    }

    fn finalize_acquire(
        &self,
        surface_id: SurfaceId,
        write: bool,
        snap: &AcquireSnapshot,
    ) -> Result<(), AdapterError> {
        // Wait for prior work to drain.
        if snap
            .timeline
            .wait(snap.wait_value, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            self.rollback(surface_id, write);
            return Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            });
        }

        // Acquire-time logging — customers know they paid for this.
        let total_bytes: u64 = snap.planes.iter().map(|p| p.byte_size).sum();
        tracing::info!(
            surface_id = surface_id,
            width = snap.width,
            height = snap.height,
            format = ?snap.format,
            plane_count = snap.planes.len(),
            bytes = total_bytes,
            mode = if write { "write" } else { "read" },
            "cpu-readback: GPU→CPU copy of {}x{} {:?} surface, {} bytes total ({} planes)",
            snap.width,
            snap.height,
            snap.format,
            total_bytes,
            snap.planes.len(),
        );

        // Issue: image (current layout) → TRANSFER_SRC_OPTIMAL → copy
        //        → image (TRANSFER_SRC_OPTIMAL → GENERAL).
        if let Err(err) = self.copy_image_to_buffer(snap) {
            self.rollback(surface_id, write);
            return Err(err);
        }

        // Image is in GENERAL after the copy path.
        let mut map = self.surfaces.lock();
        if let Some(state) = map.get_mut(&surface_id) {
            state.current_layout = VulkanLayout::GENERAL;
        }
        Ok(())
    }

    fn rollback(&self, surface_id: SurfaceId, write: bool) {
        let mut map = self.surfaces.lock();
        if let Some(state) = map.get_mut(&surface_id) {
            if write {
                state.write_held = false;
            } else {
                state.read_holders = state.read_holders.saturating_sub(1);
            }
        }
    }

    /// Submit a one-shot command buffer that:
    ///   - transitions the image (`from` → `TRANSFER_SRC_OPTIMAL`) using
    ///     the format-correct aspect mask
    ///   - `vkCmdCopyImageToBuffer` into the per-plane staging buffers
    ///     (one region per plane with the corresponding aspect mask)
    ///   - transitions the image (`TRANSFER_SRC_OPTIMAL` → `GENERAL`)
    /// then blocks via `vkQueueWaitIdle` so the host bytes are
    /// observable.
    ///
    /// `vkQueueWaitIdle` is a **queue-wide** stall — every other
    /// workload sharing this queue (encoder, decoder, camera) blocks
    /// until the copy completes. That's correct for a v1
    /// "GPU→CPU is the explicit slow exit" adapter, but the steady
    /// state should switch to a per-submit fence + timeline wait so
    /// only this surface's pipeline stalls. Tracked as part of the
    /// adapter runtime-integration follow-up issues filed against
    /// the Surface Adapter Architecture milestone.
    fn copy_image_to_buffer(&self, snap: &AcquireSnapshot) -> Result<(), AdapterError> {
        let device = self.device.device();
        let queue = self.device.queue();
        let qf = self.device.queue_family_index();
        let combined_aspect = combined_aspect_mask(snap.format);

        let (pool, cmd) = create_one_shot_command_buffer(device, qf)?;

        let begin_info = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();
        if let Err(e) = unsafe { device.begin_command_buffer(cmd, &begin_info) } {
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("begin_command_buffer: {e}"),
            });
        }

        let pre_barrier = build_image_barrier(
            snap.image,
            qf,
            snap.from.vk(),
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            combined_aspect,
        );
        let pre_barriers = [pre_barrier];
        let pre_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&pre_barriers)
            .build();
        unsafe { device.cmd_pipeline_barrier2(cmd, &pre_dep) };

        // One copy per plane. NV12 issues two regions (Y → plane-0
        // staging buffer with PLANE_0 aspect; UV → plane-1 staging buffer
        // with PLANE_1 aspect). Single-plane formats issue one region
        // with the COLOR aspect.
        for (plane_idx, plane) in snap.planes.iter().enumerate() {
            let aspect = plane_aspect_mask(snap.format, plane_idx as u32);
            let copy_region = vk::BufferImageCopy::builder()
                .buffer_offset(0)
                // Tight packing: row length = plane width texels,
                // image height = plane height rows.
                .buffer_row_length(plane.width)
                .buffer_image_height(plane.height)
                .image_subresource(
                    vk::ImageSubresourceLayers::builder()
                        .aspect_mask(aspect)
                        .mip_level(0)
                        .base_array_layer(0)
                        .layer_count(1)
                        .build(),
                )
                .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .image_extent(vk::Extent3D {
                    width: plane.width,
                    height: plane.height,
                    depth: 1,
                })
                .build();
            unsafe {
                device.cmd_copy_image_to_buffer(
                    cmd,
                    snap.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    plane.buffer,
                    &[copy_region],
                )
            };
        }

        // Image: TRANSFER_SRC_OPTIMAL → GENERAL (deterministic post-state).
        // Each per-plane staging VkBuffer: TRANSFER_WRITE → HOST_READ so
        // the unmapped bytes are host-coherent after the wait.
        let post_barrier = build_image_barrier(
            snap.image,
            qf,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::ImageLayout::GENERAL,
            combined_aspect,
        );
        let post_barriers = [post_barrier];
        let post_buf_barriers: Vec<vk::BufferMemoryBarrier2> = snap
            .planes
            .iter()
            .map(|p| {
                vk::BufferMemoryBarrier2::builder()
                    .src_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
                    .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                    .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                    .dst_access_mask(vk::AccessFlags2::HOST_READ)
                    .buffer(p.buffer)
                    .offset(0)
                    .size(vk::WHOLE_SIZE)
                    .build()
            })
            .collect();
        let post_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&post_barriers)
            .buffer_memory_barriers(&post_buf_barriers)
            .build();
        unsafe { device.cmd_pipeline_barrier2(cmd, &post_dep) };

        if let Err(e) = unsafe { device.end_command_buffer(cmd) } {
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("end_command_buffer: {e}"),
            });
        }

        let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cmd)
            .build()];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cmd_infos)
            .build();
        if let Err(e) =
            unsafe { self.device.submit_to_queue(queue, &[submit], vk::Fence::null()) }
        {
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("submit_to_queue: {e}"),
            });
        }

        if let Err(e) = unsafe { device.queue_wait_idle(queue) } {
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("queue_wait_idle: {e}"),
            });
        }

        unsafe { device.destroy_command_pool(pool, None) };
        Ok(())
    }

    /// Symmetric counterpart of [`Self::copy_image_to_buffer`] — flushes
    /// each plane's linear staging buffer back into the corresponding
    /// image plane and leaves the image in `GENERAL`. Called from the
    /// WRITE guard's release path.
    fn copy_buffer_to_image(&self, snap: &FlushSnapshot) -> Result<(), AdapterError> {
        let device = self.device.device();
        let queue = self.device.queue();
        let qf = self.device.queue_family_index();
        let combined_aspect = combined_aspect_mask(snap.format);

        let (pool, cmd) = create_one_shot_command_buffer(device, qf)?;

        let begin_info = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();
        if let Err(e) = unsafe { device.begin_command_buffer(cmd, &begin_info) } {
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("begin_command_buffer: {e}"),
            });
        }

        let pre_barrier = build_image_barrier(
            snap.image,
            qf,
            snap.from.vk(),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            combined_aspect,
        );
        let pre_barriers = [pre_barrier];
        let pre_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&pre_barriers)
            .build();
        unsafe { device.cmd_pipeline_barrier2(cmd, &pre_dep) };

        for (plane_idx, plane) in snap.planes.iter().enumerate() {
            let aspect = plane_aspect_mask(snap.format, plane_idx as u32);
            let copy_region = vk::BufferImageCopy::builder()
                .buffer_offset(0)
                .buffer_row_length(plane.width)
                .buffer_image_height(plane.height)
                .image_subresource(
                    vk::ImageSubresourceLayers::builder()
                        .aspect_mask(aspect)
                        .mip_level(0)
                        .base_array_layer(0)
                        .layer_count(1)
                        .build(),
                )
                .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .image_extent(vk::Extent3D {
                    width: plane.width,
                    height: plane.height,
                    depth: 1,
                })
                .build();
            unsafe {
                device.cmd_copy_buffer_to_image(
                    cmd,
                    plane.buffer,
                    snap.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[copy_region],
                )
            };
        }

        let post_barrier = build_image_barrier(
            snap.image,
            qf,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::GENERAL,
            combined_aspect,
        );
        let post_barriers = [post_barrier];
        let post_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&post_barriers)
            .build();
        unsafe { device.cmd_pipeline_barrier2(cmd, &post_dep) };

        if let Err(e) = unsafe { device.end_command_buffer(cmd) } {
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("end_command_buffer: {e}"),
            });
        }

        let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cmd)
            .build()];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cmd_infos)
            .build();
        if let Err(e) =
            unsafe { self.device.submit_to_queue(queue, &[submit], vk::Fence::null()) }
        {
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("submit_to_queue: {e}"),
            });
        }

        if let Err(e) = unsafe { device.queue_wait_idle(queue) } {
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("queue_wait_idle: {e}"),
            });
        }

        unsafe { device.destroy_command_pool(pool, None) };
        Ok(())
    }
}

/// Per-plane snapshot taken under the registry lock.
#[derive(Clone, Copy)]
struct PlaneAcquireSlot {
    buffer: vk::Buffer,
    mapped_ptr: *mut u8,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
    byte_size: u64,
}

/// Snapshot taken under the registry lock so the timeline wait + GPU
/// copy can run unlocked. `read_holders` / `write_held` are already
/// incremented; rollback paths decrement them on failure.
struct AcquireSnapshot {
    timeline: Arc<VulkanTimelineSemaphore>,
    wait_value: u64,
    image: vk::Image,
    from: VulkanLayout,
    format: SurfaceFormat,
    width: u32,
    height: u32,
    planes: Vec<PlaneAcquireSlot>,
}

// Safe: raw pointers point into HOST_VISIBLE/HOST_COHERENT mapped
// allocations that outlive the snapshot, and are only ever touched by
// the thread that owns the active acquire scope.
unsafe impl Send for AcquireSnapshot {}
unsafe impl Sync for AcquireSnapshot {}

struct FlushSnapshot {
    image: vk::Image,
    from: VulkanLayout,
    format: SurfaceFormat,
    planes: Vec<PlaneAcquireSlot>,
}

unsafe impl Send for FlushSnapshot {}
unsafe impl Sync for FlushSnapshot {}

fn create_one_shot_command_buffer(
    device: &vulkanalia::Device,
    qf: u32,
) -> Result<(vk::CommandPool, vk::CommandBuffer), AdapterError> {
    let pool_info = vk::CommandPoolCreateInfo::builder()
        .queue_family_index(qf)
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .build();
    let pool =
        unsafe { device.create_command_pool(&pool_info, None) }.map_err(|e| {
            AdapterError::IpcDisconnected {
                reason: format!("create_command_pool: {e}"),
            }
        })?;

    let alloc_info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1)
        .build();
    let cmd_buffers = match unsafe { device.allocate_command_buffers(&alloc_info) } {
        Ok(v) => v,
        Err(e) => {
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("allocate_command_buffers: {e}"),
            });
        }
    };
    Ok((pool, cmd_buffers[0]))
}

fn build_image_barrier(
    image: vk::Image,
    qf: u32,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
    aspect_mask: vk::ImageAspectFlags,
) -> vk::ImageMemoryBarrier2 {
    vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
        .old_layout(from)
        .new_layout(to)
        .src_queue_family_index(qf)
        .dst_queue_family_index(qf)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::builder()
                .aspect_mask(aspect_mask)
                .base_mip_level(0)
                .level_count(1)
                .base_array_layer(0)
                .layer_count(1)
                .build(),
        )
        .build()
}

/// Vulkan aspect mask for the given plane of a [`SurfaceFormat`], suitable
/// for `VkImageSubresourceLayers` when issuing image↔buffer copies.
fn plane_aspect_mask(format: SurfaceFormat, plane: u32) -> vk::ImageAspectFlags {
    match (format, plane) {
        (SurfaceFormat::Bgra8 | SurfaceFormat::Rgba8, 0) => vk::ImageAspectFlags::COLOR,
        (SurfaceFormat::Nv12, 0) => vk::ImageAspectFlags::PLANE_0,
        (SurfaceFormat::Nv12, 1) => vk::ImageAspectFlags::PLANE_1,
        _ => unreachable!("plane_aspect_mask: plane {plane} out of range for {format:?}"),
    }
}

/// Aspect mask covering every plane of a [`SurfaceFormat`], for
/// `VkImageMemoryBarrier::subresourceRange`. Vulkan requires the barrier
/// to cover all aspects the image possesses; for multi-plane images that
/// is `PLANE_0 | PLANE_1 | …`, not `COLOR`.
fn combined_aspect_mask(format: SurfaceFormat) -> vk::ImageAspectFlags {
    match format {
        SurfaceFormat::Bgra8 | SurfaceFormat::Rgba8 => vk::ImageAspectFlags::COLOR,
        SurfaceFormat::Nv12 => vk::ImageAspectFlags::PLANE_0 | vk::ImageAspectFlags::PLANE_1,
    }
}

impl SurfaceAdapter for CpuReadbackSurfaceAdapter {
    type ReadView<'g> = CpuReadbackReadView<'g>;
    type WriteView<'g> = CpuReadbackWriteView<'g>;

    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        let snap = match self.try_begin(surface, false)? {
            Some(s) => s,
            None => {
                return Err(AdapterError::WriteContended {
                    surface_id: surface.id,
                    holder: "writer".to_string(),
                });
            }
        };
        self.finalize_acquire(surface.id, false, &snap)?;
        Ok(ReadGuard::new(self, surface.id, build_read_view(&snap)))
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        let snap = match self.try_begin(surface, true)? {
            Some(s) => s,
            None => {
                let map = self.surfaces.lock();
                let holder = match map.get(&surface.id) {
                    Some(s) if s.write_held => "writer".to_string(),
                    Some(s) => format!("{} reader(s)", s.read_holders),
                    None => "unknown".to_string(),
                };
                drop(map);
                return Err(AdapterError::WriteContended {
                    surface_id: surface.id,
                    holder,
                });
            }
        };
        self.finalize_acquire(surface.id, true, &snap)?;
        Ok(WriteGuard::new(self, surface.id, build_write_view(&snap)))
    }

    fn try_acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError> {
        let snap = match self.try_begin(surface, false)? {
            Some(s) => s,
            None => return Ok(None),
        };
        self.finalize_acquire(surface.id, false, &snap)?;
        Ok(Some(ReadGuard::new(self, surface.id, build_read_view(&snap))))
    }

    fn try_acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError> {
        let snap = match self.try_begin(surface, true)? {
            Some(s) => s,
            None => return Ok(None),
        };
        self.finalize_acquire(surface.id, true, &snap)?;
        Ok(Some(WriteGuard::new(
            self,
            surface.id,
            build_write_view(&snap),
        )))
    }

    fn end_read_access(&self, surface_id: SurfaceId) {
        let (timeline, value) = {
            let mut map = self.surfaces.lock();
            let state = match map.get_mut(&surface_id) {
                Some(s) => s,
                None => {
                    tracing::warn!(
                        ?surface_id,
                        "end_read_access on unknown surface — racing unregister"
                    );
                    return;
                }
            };
            debug_assert!(state.read_holders > 0, "read release without acquire");
            state.read_holders = state.read_holders.saturating_sub(1);
            if state.read_holders > 0 {
                return;
            }
            let next = state.next_release_value();
            state.current_release_value = next;
            (Arc::clone(&state.timeline), next)
        };
        if let Err(e) = timeline.signal_host(value) {
            tracing::error!(?surface_id, %value, %e, "timeline signal failed on read release");
        }
    }

    fn end_write_access(&self, surface_id: SurfaceId) {
        // Snapshot the work we need to do under the lock, then run the
        // GPU copy unlocked.
        let snap = {
            let mut map = self.surfaces.lock();
            let state = match map.get_mut(&surface_id) {
                Some(s) => s,
                None => {
                    tracing::warn!(
                        ?surface_id,
                        "end_write_access on unknown surface — racing unregister"
                    );
                    return;
                }
            };
            debug_assert!(state.write_held, "write release without acquire");
            let image = match state.texture.vulkan_inner().image() {
                Some(i) => i,
                None => {
                    state.write_held = false;
                    tracing::warn!(?surface_id, "end_write_access: vulkan image unavailable");
                    return;
                }
            };
            let planes: Vec<PlaneAcquireSlot> = state
                .planes
                .iter()
                .map(|p| PlaneAcquireSlot {
                    buffer: p.staging.buffer(),
                    mapped_ptr: p.staging.mapped_ptr(),
                    width: p.width,
                    height: p.height,
                    bytes_per_pixel: p.bytes_per_pixel,
                    byte_size: p.byte_size(),
                })
                .collect();
            FlushSnapshot {
                image,
                from: state.current_layout,
                format: state.format,
                planes,
            }
        };

        if let Err(e) = self.copy_buffer_to_image(&snap) {
            tracing::error!(
                ?surface_id,
                error = %e,
                "cpu-readback flush-back (vkCmdCopyBufferToImage) failed"
            );
            // Even on copy failure, release the lock so the caller can
            // retry — leaving `write_held=true` would deadlock the surface.
            let mut map = self.surfaces.lock();
            if let Some(state) = map.get_mut(&surface_id) {
                state.write_held = false;
            }
            return;
        }

        let (timeline, value) = {
            let mut map = self.surfaces.lock();
            let state = match map.get_mut(&surface_id) {
                Some(s) => s,
                None => return,
            };
            state.write_held = false;
            state.current_layout = VulkanLayout::GENERAL;
            let next = state.next_release_value();
            state.current_release_value = next;
            (Arc::clone(&state.timeline), next)
        };
        if let Err(e) = timeline.signal_host(value) {
            tracing::error!(?surface_id, %value, %e, "timeline signal failed on write release");
        }
    }
}

fn build_read_view<'g>(snap: &AcquireSnapshot) -> CpuReadbackReadView<'g> {
    let planes = snap
        .planes
        .iter()
        .map(|p| CpuReadbackPlaneView {
            bytes: unsafe { std::slice::from_raw_parts(p.mapped_ptr, p.byte_size as usize) },
            width: p.width,
            height: p.height,
            bytes_per_pixel: p.bytes_per_pixel,
            _marker: PhantomData,
        })
        .collect();
    CpuReadbackReadView {
        format: snap.format,
        width: snap.width,
        height: snap.height,
        planes,
    }
}

fn build_write_view<'g>(snap: &AcquireSnapshot) -> CpuReadbackWriteView<'g> {
    let planes = snap
        .planes
        .iter()
        .map(|p| CpuReadbackPlaneViewMut {
            bytes: unsafe {
                std::slice::from_raw_parts_mut(p.mapped_ptr, p.byte_size as usize)
            },
            width: p.width,
            height: p.height,
            bytes_per_pixel: p.bytes_per_pixel,
            _marker: PhantomData,
        })
        .collect();
    CpuReadbackWriteView {
        format: snap.format,
        width: snap.width,
        height: snap.height,
        planes,
    }
}
