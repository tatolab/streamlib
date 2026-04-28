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
//!  4. Wait on a per-submit `vk::Fence` so the bytes are observable from
//!     the CPU side once the call returns. The wait is targeted to this
//!     submit only — no `vkQueueWaitIdle` queue-wide drain.
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

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use streamlib::adapter_support::{HostVulkanDevice, HostVulkanPixelBuffer, HostVulkanTimelineSemaphore};
use streamlib::core::rhi::PixelFormat;
use streamlib_adapter_abi::{
    AdapterError, ReadGuard, Registry, StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceId,
    SurfaceRegistration, WriteGuard,
};
use tracing::instrument;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::state::{HostSurfaceRegistration, PlaneSlot, SurfaceState, VulkanLayout};
use crate::view::{
    CpuReadbackPlaneView, CpuReadbackPlaneViewMut, CpuReadbackReadView, CpuReadbackWriteView,
};

/// Default per-acquire GPU-wait timeout. Bounds both the prior-work
/// timeline-semaphore wait and the per-submit `vk::Fence` wait that
/// observe the image↔buffer copies.
const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-plane staging-buffer view returned by
/// [`CpuReadbackSurfaceAdapter::snapshot_plane_geometry`]. The `staging`
/// `Arc` is cloned from the adapter's owned slot — keeping the buffer
/// alive while a host bridge surface-share-checks-it-in does NOT extend
/// its lifetime past surface unregister, but it does prevent the
/// adapter's slot from being torn down underneath an in-flight bridge
/// call.
pub struct CpuReadbackStagingPlane {
    pub staging: Arc<HostVulkanPixelBuffer>,
    pub width: u32,
    pub height: u32,
    pub bytes_per_pixel: u32,
}

/// Snapshot of a registered surface's plane geometry. Used by the
/// host-side bridge that wraps this adapter for the escalate-IPC seam.
pub struct CpuReadbackSurfaceSnapshot {
    pub width: u32,
    pub height: u32,
    pub format: SurfaceFormat,
    pub planes: Vec<CpuReadbackStagingPlane>,
}

/// Explicit GPU→CPU [`SurfaceAdapter`] implementation.
///
/// Construct with [`Self::new`] passing the host's [`HostVulkanDevice`].
/// Register host-allocated surfaces with [`Self::register_host_surface`];
/// each registration allocates one dedicated [`HostVulkanPixelBuffer`] per
/// plane (HOST_VISIBLE/HOST_COHERENT linear buffer, sized exactly to the
/// plane's pixel footprint) used as the staging area for image↔buffer
/// copies. Consumers acquire scoped access through the standard
/// [`SurfaceAdapter::acquire_read`] / [`SurfaceAdapter::acquire_write`]
/// API or via the [`crate::CpuReadbackContext`] convenience.
pub struct CpuReadbackSurfaceAdapter {
    device: Arc<HostVulkanDevice>,
    surfaces: Registry<SurfaceState>,
    acquire_timeout: Duration,
}

impl CpuReadbackSurfaceAdapter {
    /// Construct an empty adapter bound to `device`.
    pub fn new(device: Arc<HostVulkanDevice>) -> Self {
        Self {
            device,
            surfaces: Registry::new(),
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
        }
    }

    /// Override the per-acquire GPU-wait timeout. Bounds both the
    /// prior-work timeline-semaphore wait and the per-submit
    /// `vk::Fence` wait that observe the image↔buffer copies. Default
    /// 5 s.
    pub fn with_acquire_timeout(mut self, timeout: Duration) -> Self {
        self.acquire_timeout = timeout;
        self
    }

    /// Returns the underlying device.
    pub fn device(&self) -> &Arc<HostVulkanDevice> {
        &self.device
    }

    /// Register a host-allocated surface with this adapter.
    ///
    /// Allocates one dedicated `HostVulkanPixelBuffer` per plane (HOST_VISIBLE,
    /// HOST_COHERENT, linear). Plane geometry is derived from
    /// [`HostSurfaceRegistration::format`] via [`SurfaceFormat::plane_count`]
    /// and [`SurfaceFormat::plane_byte_size`].
    #[instrument(level = "debug", skip(self, registration), fields(surface_id = id))]
    pub fn register_host_surface(
        &self,
        id: SurfaceId,
        registration: HostSurfaceRegistration,
    ) -> Result<(), AdapterError> {
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
            // The `PixelFormat` argument to `HostVulkanPixelBuffer::new` is
            // opaque metadata only — `bytes_per_pixel` drives the
            // allocation size. We pass `Bgra32` uniformly so the
            // staging buffer's recorded format isn't claiming a specific
            // pixel layout (Y/UV/RGB) the adapter never interprets.
            let staging = Arc::new(
                HostVulkanPixelBuffer::new(&self.device, pw, ph, pbpp, PixelFormat::Bgra32).map_err(
                    |e| AdapterError::IpcDisconnected {
                        reason: format!(
                            "HostVulkanPixelBuffer::new for cpu-readback staging plane {plane_idx}: {e}"
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

        let state = SurfaceState {
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
        };
        if !self.surfaces.register(id, state) {
            // Local `state` (and its `planes` Vec) drops here, releasing
            // the staging `HostVulkanPixelBuffer`s we just allocated. Return
            // SurfaceNotFound to match the pre-Registry semantics —
            // callers reading that error treat it as "id collision".
            return Err(AdapterError::SurfaceNotFound { surface_id: id });
        }
        Ok(())
    }

    /// Drop a registered surface.
    pub fn unregister_host_surface(&self, id: SurfaceId) -> bool {
        self.surfaces.unregister(id).is_some()
    }

    /// Snapshot the registry size — primarily for tests / observability.
    pub fn registered_count(&self) -> usize {
        self.surfaces.len()
    }

    /// Blocking read acquire keyed by `SurfaceId` instead of a full
    /// [`StreamlibSurface`] descriptor. The escalate-IPC dispatch path
    /// uses this — the wire format only carries the id, and the
    /// adapter's internal per-surface state already holds everything
    /// the descriptor would supply (transport handles, sync state,
    /// dimensions, format).
    pub fn acquire_read_by_id<'g>(
        &'g self,
        surface_id: SurfaceId,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        let snap = match self.try_begin_read_inner(surface_id)? {
            Some(s) => s,
            None => {
                return Err(AdapterError::WriteContended {
                    surface_id,
                    holder: "writer".to_string(),
                });
            }
        };
        self.finalize_acquire(surface_id, false, &snap)?;
        Ok(ReadGuard::new(self, surface_id, build_read_view(&snap)))
    }

    /// Blocking write acquire keyed by `SurfaceId`.
    pub fn acquire_write_by_id<'g>(
        &'g self,
        surface_id: SurfaceId,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        let snap = match self.try_begin_write_inner(surface_id)? {
            Some(s) => s,
            None => {
                return Err(AdapterError::WriteContended {
                    surface_id,
                    holder: self.surfaces.describe_contention(surface_id),
                });
            }
        };
        self.finalize_acquire(surface_id, true, &snap)?;
        Ok(WriteGuard::new(self, surface_id, build_write_view(&snap)))
    }

    /// Non-blocking read acquire keyed by `SurfaceId`.
    pub fn try_acquire_read_by_id<'g>(
        &'g self,
        surface_id: SurfaceId,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError> {
        let snap = match self.try_begin_read_inner(surface_id)? {
            Some(s) => s,
            None => return Ok(None),
        };
        self.finalize_acquire(surface_id, false, &snap)?;
        Ok(Some(ReadGuard::new(
            self,
            surface_id,
            build_read_view(&snap),
        )))
    }

    /// Non-blocking write acquire keyed by `SurfaceId`.
    pub fn try_acquire_write_by_id<'g>(
        &'g self,
        surface_id: SurfaceId,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError> {
        let snap = match self.try_begin_write_inner(surface_id)? {
            Some(s) => s,
            None => return Ok(None),
        };
        self.finalize_acquire(surface_id, true, &snap)?;
        Ok(Some(WriteGuard::new(
            self,
            surface_id,
            build_write_view(&snap),
        )))
    }

    /// Snapshot the per-plane staging buffers for `surface_id` plus
    /// surface dimensions and format. Returns `None` if the surface is
    /// not registered. Used by the [`CpuReadbackBridge`] impl in
    /// subprocess-runtime glue to surface staging buffers via
    /// surface-share without re-deriving plane geometry.
    pub fn snapshot_plane_geometry(
        &self,
        surface_id: SurfaceId,
    ) -> Option<CpuReadbackSurfaceSnapshot> {
        self.surfaces.with(surface_id, |state| {
            let planes = state
                .planes
                .iter()
                .map(|p| CpuReadbackStagingPlane {
                    staging: Arc::clone(&p.staging),
                    width: p.width,
                    height: p.height,
                    bytes_per_pixel: p.bytes_per_pixel,
                })
                .collect();
            CpuReadbackSurfaceSnapshot {
                width: state.width,
                height: state.height,
                format: state.format,
                planes,
            }
        })
    }

    /// Snapshot the per-acquire state needed to drive
    /// `vkCmdCopyImageToBuffer`. Adapter-internal helper invoked under
    /// the registry lock; commits `read_holders++` /
    /// `write_held = true` atomically with the snapshot.
    fn snapshot_for_acquire(
        state: &mut SurfaceState,
        surface_id: SurfaceId,
    ) -> Result<AcquireSnapshot, AdapterError> {
        let timeline = Arc::clone(&state.timeline);
        let wait_value = state.current_release_value;
        let image = state
            .texture
            .vulkan_inner()
            .image()
            .ok_or(AdapterError::SurfaceNotFound { surface_id })?;
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
        Ok(AcquireSnapshot {
            timeline,
            wait_value,
            image,
            from,
            format,
            width,
            height,
            planes: plane_snaps,
        })
    }

    fn try_begin_read_inner(
        &self,
        surface_id: SurfaceId,
    ) -> Result<Option<AcquireSnapshot>, AdapterError> {
        self.surfaces
            .try_begin_read(surface_id, |state| Self::snapshot_for_acquire(state, surface_id))
    }

    fn try_begin_write_inner(
        &self,
        surface_id: SurfaceId,
    ) -> Result<Option<AcquireSnapshot>, AdapterError> {
        self.surfaces
            .try_begin_write(surface_id, |state| Self::snapshot_for_acquire(state, surface_id))
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
            self.rollback_acquire(surface_id, write);
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
            self.rollback_acquire(surface_id, write);
            return Err(err);
        }

        // Image is in GENERAL after the copy path.
        self.surfaces.with_mut(surface_id, |state| {
            state.current_layout = VulkanLayout::GENERAL;
        });
        Ok(())
    }

    /// Symmetric counter rollback for the acquire path. Forwards to
    /// the Registry's read/write rollback helpers based on `write`.
    fn rollback_acquire(&self, surface_id: SurfaceId, write: bool) {
        if write {
            self.surfaces.rollback_write(surface_id);
        } else {
            self.surfaces.rollback_read(surface_id);
        }
    }

    /// Submit a one-shot command buffer that transitions the image to
    /// `TRANSFER_SRC_OPTIMAL`, copies each plane into its staging
    /// buffer, and transitions the image to `GENERAL`. Completion is
    /// observed via a per-submit `vk::Fence` — the wait is targeted to
    /// this submit only and composes correctly with concurrent activity
    /// from other workloads sharing the queue (no `vkQueueWaitIdle`
    /// drain).
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

        let submit_result = self.submit_and_wait(queue, cmd);
        unsafe { device.destroy_command_pool(pool, None) };
        submit_result
    }

    /// Symmetric counterpart of [`Self::copy_image_to_buffer`] — flushes
    /// each plane's linear staging buffer back into the corresponding
    /// image plane and leaves the image in `GENERAL`. Called from the
    /// WRITE guard's release path. Like the readback path, completion is
    /// observed via a per-submit `vk::Fence`, not `vkQueueWaitIdle`.
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

        let submit_result = self.submit_and_wait(queue, cmd);
        unsafe { device.destroy_command_pool(pool, None) };
        submit_result
    }

    /// Submit `cmd` to `queue` with a per-call `vk::Fence` and wait for
    /// it to signal. Bounded by [`Self::acquire_timeout`]; on timeout
    /// returns [`AdapterError::SyncTimeout`].
    ///
    /// The fence is created unsignaled, attached to a single-submit
    /// `VkSubmitInfo2`, and destroyed before this returns. Does NOT call
    /// `vkQueueWaitIdle` — the wait is targeted to this submit only and
    /// composes correctly with concurrent activity from other workloads
    /// sharing the queue.
    fn submit_and_wait(
        &self,
        queue: vk::Queue,
        cmd: vk::CommandBuffer,
    ) -> Result<(), AdapterError> {
        let device = self.device.device();

        let fence_info = vk::FenceCreateInfo::builder().build();
        let fence = unsafe { device.create_fence(&fence_info, None) }.map_err(|e| {
            AdapterError::IpcDisconnected {
                reason: format!("create_fence: {e}"),
            }
        })?;

        let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cmd)
            .build()];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cmd_infos)
            .build();
        if let Err(e) = unsafe { self.device.submit_to_queue(queue, &[submit], fence) } {
            unsafe { device.destroy_fence(fence, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("submit_to_queue: {e}"),
            });
        }

        let timeout_ns = self.acquire_timeout.as_nanos() as u64;
        let wait_result = unsafe { device.wait_for_fences(&[fence], true, timeout_ns) };
        unsafe { device.destroy_fence(fence, None) };

        match wait_result {
            Ok(vk::SuccessCode::SUCCESS) => Ok(()),
            Ok(vk::SuccessCode::TIMEOUT) => Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            }),
            Ok(other) => Err(AdapterError::IpcDisconnected {
                reason: format!("wait_for_fences unexpected success: {other:?}"),
            }),
            Err(e) => Err(AdapterError::IpcDisconnected {
                reason: format!("wait_for_fences: {e}"),
            }),
        }
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
    timeline: Arc<HostVulkanTimelineSemaphore>,
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
        self.acquire_read_by_id(surface.id)
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        self.acquire_write_by_id(surface.id)
    }

    fn try_acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError> {
        self.try_acquire_read_by_id(surface.id)
    }

    fn try_acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError> {
        self.try_acquire_write_by_id(surface.id)
    }

    fn end_read_access(&self, surface_id: SurfaceId) {
        // Inner Option: `None` means "not the last reader, skip signal".
        // Outer Option: `None` means the surface raced an unregister.
        let signal = self.surfaces.with_mut(surface_id, |state| {
            debug_assert!(state.read_holders > 0, "read release without acquire");
            state.dec_read_holders();
            if state.read_holders > 0 {
                return None;
            }
            let next = state.next_release_value();
            state.current_release_value = next;
            Some((Arc::clone(&state.timeline), next))
        });
        let signal = match signal {
            Some(s) => s,
            None => {
                tracing::warn!(
                    ?surface_id,
                    "end_read_access on unknown surface — racing unregister"
                );
                return;
            }
        };
        if let Some((timeline, value)) = signal {
            if let Err(e) = timeline.signal_host(value) {
                tracing::error!(?surface_id, %value, %e, "timeline signal failed on read release");
            }
        }
    }

    fn end_write_access(&self, surface_id: SurfaceId) {
        // Snapshot the work we need to do under the lock, then run the
        // GPU copy unlocked. Outer Option: surface raced an unregister.
        // Inner Option: surface exists but its vulkan image is gone —
        // we still clear write_held and bail.
        let snap = self.surfaces.with_mut(surface_id, |state| {
            debug_assert!(state.write_held, "write release without acquire");
            let image = match state.texture.vulkan_inner().image() {
                Some(i) => i,
                None => {
                    state.set_write_held(false);
                    return None;
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
            Some(FlushSnapshot {
                image,
                from: state.current_layout,
                format: state.format,
                planes,
            })
        });
        let snap = match snap {
            Some(Some(s)) => s,
            Some(None) => {
                tracing::warn!(?surface_id, "end_write_access: vulkan image unavailable");
                return;
            }
            None => {
                tracing::warn!(
                    ?surface_id,
                    "end_write_access on unknown surface — racing unregister"
                );
                return;
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
            self.surfaces.rollback_write(surface_id);
            return;
        }

        let signal = self.surfaces.with_mut(surface_id, |state| {
            state.set_write_held(false);
            state.current_layout = VulkanLayout::GENERAL;
            let next = state.next_release_value();
            state.current_release_value = next;
            (Arc::clone(&state.timeline), next)
        });
        let (timeline, value) = match signal {
            Some(s) => s,
            None => return,
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
