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

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use streamlib_adapter_abi::{
    AdapterError, ReadGuard, Registry, StreamlibSurface, SurfaceAdapter, SurfaceId,
    SurfaceRegistration, WriteGuard,
};
use streamlib_consumer_rhi::{
    DevicePrivilege, VulkanPixelBufferLike, VulkanRhiDevice, VulkanTimelineSemaphoreLike,
};
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
