// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `CudaSurfaceAdapter<D>` — CUDA-typed `SurfaceAdapter`.
//!
//! Generic over the device flavor: `D = HostVulkanDevice` for host-side
//! adapter use today (allocate OPAQUE_FD-exportable resources, register,
//! hand fds to CUDA). The four trait methods on `VulkanRhiDevice`
//! (`device()`, `queue()`, `queue_family_index()`, `submit_to_queue()`)
//! are everything the adapter needs from the device; the timeline-
//! semaphore type is picked up via `D::Privilege::TimelineSemaphore`
//! and abstracted behind `VulkanTimelineSemaphoreLike` so wait + signal
//! works against either flavor.
//!
//! The adapter:
//! - Owns a registry of registered surfaces keyed by [`SurfaceId`].
//! - Waits on the timeline semaphore at the start of every acquire so
//!   prior CUDA / GPU work has drained before the host touches the
//!   buffer.
//! - Signals the next timeline value on guard drop so the next acquire
//!   wakes up.
//!
//! Per-acquire host *work* (e.g. `vkCmdCopyImageToBuffer`) is **not
//! present** here, on purpose: CUDA imports the OPAQUE_FD memory once at
//! registration time and dispatches kernels in its own context. The
//! timeline semaphore is the only sync surface that has to cross the
//! Vulkan↔CUDA boundary per acquire.
//!
//! For the *host-pipeline producer* shape (in-process processor pushing
//! frames into a registered surface so a subprocess customer can
//! `acquire_read` them GPU-resident), see
//! [`CudaSurfaceAdapter::submit_host_copy_image_to_buffer`]. That path
//! is GPU-signaled and stays out of the per-acquire codepath.

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use streamlib_adapter_abi::{
    AdapterError, ReadGuard, Registry, StreamlibSurface, SurfaceAdapter, SurfaceId,
    SurfaceRegistration, WriteGuard,
};
use streamlib_consumer_rhi::{
    DevicePrivilege, VulkanLayout, VulkanPixelBufferLike, VulkanRhiDevice, VulkanTextureLike,
    VulkanTimelineSemaphoreLike,
};
#[cfg(target_os = "linux")]
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::state::{HostSurfaceRegistration, SurfaceState};
use crate::view::{CudaReadView, CudaWriteView};

/// Default per-acquire timeline-wait timeout. Long enough to cover any
/// realistic GPU queue depth; short enough that a deadlock turns into
/// an `AdapterError::SyncTimeout` rather than wedging the consumer.
const DEFAULT_TIMELINE_WAIT: Duration = Duration::from_secs(5);

/// CUDA-targeted [`SurfaceAdapter`] implementation. Generic over the
/// device flavor — instantiate as
/// `CudaSurfaceAdapter<HostVulkanDevice>` host-side; future cdylib work
/// (#589/#590) will add a consumer-flavor instantiation.
pub struct CudaSurfaceAdapter<D: VulkanRhiDevice> {
    device: Arc<D>,
    surfaces: Registry<SurfaceState<D::Privilege>>,
    /// Per-acquire timeline-wait timeout. Adjustable via
    /// [`Self::with_acquire_timeout`].
    acquire_timeout: Duration,
}

impl<D: VulkanRhiDevice> CudaSurfaceAdapter<D> {
    /// Construct an empty adapter bound to `device`.
    pub fn new(device: Arc<D>) -> Self {
        Self {
            device,
            surfaces: Registry::new(),
            acquire_timeout: DEFAULT_TIMELINE_WAIT,
        }
    }

    /// Override the per-acquire timeline-wait timeout. Default 5 s.
    pub fn with_acquire_timeout(mut self, timeout: Duration) -> Self {
        self.acquire_timeout = timeout;
        self
    }

    /// Returns the underlying device for callers (test harnesses, the
    /// `CudaContext`, raw-handle escape hatches) that need it.
    pub fn device(&self) -> &Arc<D> {
        &self.device
    }

    /// Register an allocated (or imported) surface with this adapter.
    ///
    /// `id` is assigned by the caller (typically from the surface-share
    /// service); it MUST be unique across the adapter's lifetime.
    /// Returns [`AdapterError::SurfaceAlreadyRegistered`] if `id` is
    /// already registered.
    pub fn register_host_surface(
        &self,
        id: SurfaceId,
        registration: HostSurfaceRegistration<D::Privilege>,
    ) -> Result<(), AdapterError> {
        let inserted = self.surfaces.register(
            id,
            SurfaceState {
                surface_id: id,
                pixel_buffer: registration.pixel_buffer,
                timeline: registration.timeline,
                current_layout: registration.initial_layout,
                read_holders: 0,
                write_held: false,
                current_release_value: 0,
            },
        );
        if !inserted {
            return Err(AdapterError::SurfaceAlreadyRegistered { surface_id: id });
        }
        Ok(())
    }

    /// Drop a registered surface. Pending guards keep the underlying
    /// `Arc<TimelineSemaphore>` and `Arc<PixelBuffer>` alive; the next
    /// acquire returns [`AdapterError::SurfaceNotFound`].
    pub fn unregister_host_surface(&self, id: SurfaceId) -> bool {
        self.surfaces.unregister(id).is_some()
    }

    /// Snapshot the registry size — primarily for tests and
    /// observability.
    pub fn registered_count(&self) -> usize {
        self.surfaces.len()
    }

    /// Power-user accessor: the registered pixel-buffer Arc for a
    /// surface, if registered. Used by the carve-out test in
    /// `streamlib-adapter-cuda-helpers` to call
    /// `export_opaque_fd_memory()` on the underlying buffer; future
    /// cdylib work will route this through the surface-share service
    /// instead. Returns `None` when the surface isn't registered.
    pub fn surface_pixel_buffer(
        &self,
        id: SurfaceId,
    ) -> Option<Arc<<D::Privilege as DevicePrivilege>::PixelBuffer>> {
        self.surfaces
            .with(id, |state| Arc::clone(&state.pixel_buffer))
    }

    /// Power-user accessor: the registered timeline-semaphore Arc for
    /// a surface. Used by the carve-out test to call
    /// `export_opaque_fd()` on the underlying timeline; cdylib work
    /// will route this through the surface-share service.
    pub fn surface_timeline(
        &self,
        id: SurfaceId,
    ) -> Option<Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore>> {
        self.surfaces
            .with(id, |state| Arc::clone(&state.timeline))
    }

    /// Host-pipeline producer path: submit a `vkCmdCopyImageToBuffer`
    /// from `texture` into the surface's registered buffer, with the
    /// timeline GPU-signaled at completion.
    ///
    /// Atomically takes the write lock, computes the next release
    /// value, submits the copy with a timeline signal at that value,
    /// and updates the adapter's release value so subsequent
    /// `acquire_read` calls block on the GPU signal before reading.
    ///
    /// Use case: an in-process producer (camera → cuda copy processor)
    /// pushes frames into a registered cuda surface so a subprocess
    /// AI-inference customer can `acquire_read` them without a CPU
    /// round-trip.
    ///
    /// `texture` is read via [`VulkanTextureLike`] so the caller doesn't
    /// have to import `vulkanalia` — host- and consumer-flavor textures
    /// both implement the trait. `image_layout` is both the layout the
    /// source image is currently in AND the layout it will be returned
    /// to after the copy — the caller's pipeline keeps full ownership
    /// of the layout outside the copy window.
    ///
    /// **Concurrency contract**: the caller must guarantee no other
    /// pipeline stage is operating on `texture` while this method runs.
    /// Camera ring-texture producers satisfy this trivially because each
    /// ring slot is written exactly once per frame and re-used only
    /// after a full ring rotation.
    ///
    /// Errors:
    /// - [`AdapterError::SurfaceNotFound`] — `id` not registered.
    /// - [`AdapterError::WriteContended`] — another writer or reader
    ///   holds the surface. Host writers serialize through this method.
    /// - [`AdapterError::BackendRejected`] — driver refused the submit
    ///   or the texture has no `VkImage`.
    #[cfg(target_os = "linux")]
    pub fn submit_host_copy_image_to_buffer<T>(
        &self,
        id: SurfaceId,
        texture: &T,
        image_layout: VulkanLayout,
    ) -> Result<u64, AdapterError>
    where
        T: VulkanTextureLike + ?Sized,
    {
        let image = texture.image().ok_or_else(|| AdapterError::BackendRejected {
            reason:
                "submit_host_copy_image_to_buffer: source texture has no VkImage (placeholder?)"
                    .into(),
        })?;
        let image_extent = vk::Extent3D {
            width: texture.width(),
            height: texture.height(),
            depth: 1,
        };

        // Conservative dst-bound check: the source texture's pixel
        // count must fit in the destination buffer. We don't know the
        // pixel buffer's bytes-per-pixel from inside the registry
        // closure (the trait doesn't expose it), so we use the buffer
        // size in bytes vs. width*height*4 — Bgra32 is the only
        // OPAQUE_FD format the host allocator emits today
        // (vulkan_pixel_buffer.rs::new_opaque_fd_export_device_local
        // hardcodes bytes_per_pixel=4 in the example caller). If a
        // future caller uses a non-4-bpp format, extend this check.
        let required_bytes = (image_extent.width as u64)
            .saturating_mul(image_extent.height as u64)
            .saturating_mul(4);

        let session: HostCopySession<D::Privilege> = self
            .surfaces
            .try_begin_write(id, |state| {
                let buffer_size = state.pixel_buffer.size();
                if buffer_size < required_bytes {
                    return Err(AdapterError::BackendRejected {
                        reason: format!(
                            "submit_host_copy_image_to_buffer: source texture is \
                             {}x{}x4 = {} bytes; destination cuda buffer size is \
                             {} bytes (texture would overrun)",
                            image_extent.width,
                            image_extent.height,
                            required_bytes,
                            buffer_size,
                        ),
                    });
                }
                let signal_value = state.next_release_value();
                Ok(HostCopySession {
                    timeline: Arc::clone(&state.timeline),
                    buffer: state.pixel_buffer.buffer(),
                    signal_value,
                })
            })?
            .ok_or_else(|| AdapterError::WriteContended {
                surface_id: id,
                holder: self.surfaces.describe_contention(id),
            })?;

        if let Err(e) = submit_image_to_buffer_copy_signal_timeline::<D>(
            self.device.as_ref(),
            image,
            image_layout.as_vk(),
            session.buffer,
            image_extent,
            session.timeline.as_ref(),
            session.signal_value,
        ) {
            self.surfaces.rollback_write(id);
            return Err(e);
        }

        // GPU will signal at signal_value asynchronously; record the
        // value under the registry lock and clear the write flag so
        // subsequent `acquire_read` calls wait on the right value.
        // `with_mut` returns `None` only if the surface raced an
        // unregister between our submit and now — extremely narrow,
        // but log it so the symptom is observable.
        if self
            .surfaces
            .with_mut(id, |state| {
                state.set_write_held(false);
                state.current_release_value = session.signal_value;
            })
            .is_none()
        {
            tracing::warn!(
                ?id,
                signal_value = session.signal_value,
                "submit_host_copy_image_to_buffer: surface raced unregister after submit"
            );
        }

        Ok(session.signal_value)
    }

    fn try_begin_read(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadAcquired<D::Privilege>>, AdapterError> {
        let id = surface.id;
        self.surfaces.try_begin_read(id, |state| {
            let timeline = Arc::clone(&state.timeline);
            let wait_value = state.current_release_value;
            let buffer = state.pixel_buffer.buffer();
            let size = state.pixel_buffer.size();
            Ok(ReadAcquired {
                timeline,
                wait_value,
                buffer,
                size,
            })
        })
    }

    fn try_begin_write(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteAcquired<D::Privilege>>, AdapterError> {
        let id = surface.id;
        self.surfaces.try_begin_write(id, |state| {
            let timeline = Arc::clone(&state.timeline);
            let wait_value = state.current_release_value;
            let buffer = state.pixel_buffer.buffer();
            let size = state.pixel_buffer.size();
            Ok(WriteAcquired {
                timeline,
                wait_value,
                buffer,
                size,
            })
        })
    }

    fn finalize_read(
        &self,
        surface_id: SurfaceId,
        acquired: ReadAcquired<D::Privilege>,
    ) -> Result<(vk::Buffer, vk::DeviceSize), AdapterError> {
        if acquired
            .timeline
            .wait(acquired.wait_value, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            self.surfaces.rollback_read(surface_id);
            return Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            });
        }
        Ok((acquired.buffer, acquired.size))
    }

    fn finalize_write(
        &self,
        surface_id: SurfaceId,
        acquired: WriteAcquired<D::Privilege>,
    ) -> Result<(vk::Buffer, vk::DeviceSize), AdapterError> {
        if acquired
            .timeline
            .wait(acquired.wait_value, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            self.surfaces.rollback_write(surface_id);
            return Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            });
        }
        Ok((acquired.buffer, acquired.size))
    }
}

/// Snapshot taken under the registry lock so the GPU submit can run
/// unlocked. `write_held` is already set; the rollback path clears it.
struct HostCopySession<P: DevicePrivilege> {
    timeline: Arc<P::TimelineSemaphore>,
    buffer: vk::Buffer,
    signal_value: u64,
}

/// Build a one-shot command buffer that transitions `image` into
/// `TRANSFER_SRC_OPTIMAL`, copies its full color plane into `buffer`,
/// transitions back to `image_layout`, and submits with a GPU-side
/// timeline signal at `signal_value`.
///
/// The post-copy buffer barrier is intentionally absent: cuda imports
/// the buffer's memory once at registration and reads it through
/// `cudaWaitExternalSemaphore` on the same timeline, not via host
/// `vkMapMemory`, so a HOST_READ barrier would be both unnecessary
/// and wrong.
#[cfg(target_os = "linux")]
fn submit_image_to_buffer_copy_signal_timeline<D>(
    device: &D,
    image: vk::Image,
    image_layout: vk::ImageLayout,
    buffer: vk::Buffer,
    image_extent: vk::Extent3D,
    timeline: &<D::Privilege as DevicePrivilege>::TimelineSemaphore,
    signal_value: u64,
) -> Result<(), AdapterError>
where
    D: VulkanRhiDevice,
{
    let vk_device = device.device();
    let queue = device.queue();
    let qf = device.queue_family_index();
    let transfer_layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;

    let pool_info = vk::CommandPoolCreateInfo::builder()
        .queue_family_index(qf)
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .build();
    let pool = unsafe { vk_device.create_command_pool(&pool_info, None) }.map_err(|e| {
        AdapterError::BackendRejected {
            reason: format!("create_command_pool: {e}"),
        }
    })?;

    let alloc_info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1)
        .build();
    let cmd = match unsafe { vk_device.allocate_command_buffers(&alloc_info) } {
        Ok(v) => v[0],
        Err(e) => {
            unsafe { vk_device.destroy_command_pool(pool, None) };
            return Err(AdapterError::BackendRejected {
                reason: format!("allocate_command_buffers: {e}"),
            });
        }
    };

    let begin_info = vk::CommandBufferBeginInfo::builder()
        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
        .build();
    if let Err(e) = unsafe { vk_device.begin_command_buffer(cmd, &begin_info) } {
        unsafe { vk_device.destroy_command_pool(pool, None) };
        return Err(AdapterError::BackendRejected {
            reason: format!("begin_command_buffer: {e}"),
        });
    }

    let pre_barrier = build_color_image_barrier(image, qf, image_layout, transfer_layout);
    let pre_barriers = [pre_barrier];
    let pre_dep = vk::DependencyInfo::builder()
        .image_memory_barriers(&pre_barriers)
        .build();
    unsafe { vk_device.cmd_pipeline_barrier2(cmd, &pre_dep) };

    let copy_region = vk::BufferImageCopy::builder()
        .buffer_offset(0)
        .buffer_row_length(image_extent.width)
        .buffer_image_height(image_extent.height)
        .image_subresource(
            vk::ImageSubresourceLayers::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .mip_level(0)
                .base_array_layer(0)
                .layer_count(1)
                .build(),
        )
        .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
        .image_extent(image_extent)
        .build();
    unsafe {
        vk_device.cmd_copy_image_to_buffer(cmd, image, transfer_layout, buffer, &[copy_region]);
    }

    let post_barrier = build_color_image_barrier(image, qf, transfer_layout, image_layout);
    let post_barriers = [post_barrier];
    let post_dep = vk::DependencyInfo::builder()
        .image_memory_barriers(&post_barriers)
        .build();
    unsafe { vk_device.cmd_pipeline_barrier2(cmd, &post_dep) };

    if let Err(e) = unsafe { vk_device.end_command_buffer(cmd) } {
        unsafe { vk_device.destroy_command_pool(pool, None) };
        return Err(AdapterError::BackendRejected {
            reason: format!("end_command_buffer: {e}"),
        });
    }

    let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
        .command_buffer(cmd)
        .build()];
    let signal_infos = [vk::SemaphoreSubmitInfo::builder()
        .semaphore(timeline.semaphore())
        .value(signal_value)
        .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .build()];
    let submit = vk::SubmitInfo2::builder()
        .command_buffer_infos(&cmd_infos)
        .signal_semaphore_infos(&signal_infos)
        .build();

    let submit_result = unsafe { device.submit_to_queue(queue, &[submit], vk::Fence::null()) };
    unsafe { vk_device.destroy_command_pool(pool, None) };
    submit_result.map_err(|e| AdapterError::BackendRejected {
        reason: format!("submit_to_queue: {e}"),
    })
}

#[cfg(target_os = "linux")]
fn build_color_image_barrier(
    image: vk::Image,
    qf: u32,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
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
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .base_mip_level(0)
                .level_count(1)
                .base_array_layer(0)
                .layer_count(1)
                .build(),
        )
        .build()
}

/// Snapshot taken under the registry lock so the timeline wait can run
/// unlocked. `read_holders` / `write_held` are already incremented;
/// rollback paths decrement them on failure.
struct ReadAcquired<P: DevicePrivilege> {
    timeline: Arc<P::TimelineSemaphore>,
    wait_value: u64,
    buffer: vk::Buffer,
    size: vk::DeviceSize,
}

struct WriteAcquired<P: DevicePrivilege> {
    timeline: Arc<P::TimelineSemaphore>,
    wait_value: u64,
    buffer: vk::Buffer,
    size: vk::DeviceSize,
}

impl<D: VulkanRhiDevice + 'static> SurfaceAdapter for CudaSurfaceAdapter<D> {
    type ReadView<'g> = CudaReadView<'g>;
    type WriteView<'g> = CudaWriteView<'g>;

    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        let acquired = match self.try_begin_read(surface)? {
            Some(a) => a,
            None => {
                return Err(AdapterError::WriteContended {
                    surface_id: surface.id,
                    holder: "writer".to_string(),
                });
            }
        };
        let (buffer, size) = self.finalize_read(surface.id, acquired)?;
        Ok(ReadGuard::new(
            self,
            surface.id,
            CudaReadView {
                buffer,
                size,
                _marker: PhantomData,
            },
        ))
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        let acquired = match self.try_begin_write(surface)? {
            Some(a) => a,
            None => {
                return Err(AdapterError::WriteContended {
                    surface_id: surface.id,
                    holder: self.surfaces.describe_contention(surface.id),
                });
            }
        };
        let (buffer, size) = self.finalize_write(surface.id, acquired)?;
        Ok(WriteGuard::new(
            self,
            surface.id,
            CudaWriteView {
                buffer,
                size,
                _marker: PhantomData,
            },
        ))
    }

    fn try_acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError> {
        let acquired = match self.try_begin_read(surface)? {
            Some(a) => a,
            None => return Ok(None),
        };
        let (buffer, size) = self.finalize_read(surface.id, acquired)?;
        Ok(Some(ReadGuard::new(
            self,
            surface.id,
            CudaReadView {
                buffer,
                size,
                _marker: PhantomData,
            },
        )))
    }

    fn try_acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError> {
        let acquired = match self.try_begin_write(surface)? {
            Some(a) => a,
            None => return Ok(None),
        };
        let (buffer, size) = self.finalize_write(surface.id, acquired)?;
        Ok(Some(WriteGuard::new(
            self,
            surface.id,
            CudaWriteView {
                buffer,
                size,
                _marker: PhantomData,
            },
        )))
    }

    fn end_read_access(&self, surface_id: SurfaceId) {
        // Inner Option: `None` means "not the last reader, skip signal".
        // Outer Option: `None` means "surface raced an unregister".
        let signal: Option<Option<(Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore>, u64)>> =
            self.surfaces.with_mut(surface_id, |state| {
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
        let signal: Option<(Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore>, u64)> =
            self.surfaces.with_mut(surface_id, |state| {
                debug_assert!(state.write_held, "write release without acquire");
                state.set_write_held(false);
                let next = state.next_release_value();
                state.current_release_value = next;
                (Arc::clone(&state.timeline), next)
            });
        let (timeline, value) = match signal {
            Some(s) => s,
            None => {
                tracing::warn!(
                    ?surface_id,
                    "end_write_access on unknown surface — racing unregister"
                );
                return;
            }
        };
        if let Err(e) = timeline.signal_host(value) {
            tracing::error!(?surface_id, %value, %e, "timeline signal failed on write release");
        }
    }
}
