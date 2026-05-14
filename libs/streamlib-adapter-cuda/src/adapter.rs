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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use streamlib_adapter_abi::{
    AdapterError, ReadGuard, Registry, StreamlibSurface, SurfaceAdapter, SurfaceId,
    SurfaceRegistration, WriteGuard,
};
use streamlib_consumer_rhi::{
    DevicePrivilege, TextureFormat, VulkanLayout, VulkanRhiBuffer, VulkanRhiDevice,
    VulkanTextureLike, VulkanTimelineSemaphoreLike,
};
#[cfg(target_os = "linux")]
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::state::{
    HostImageSurfaceRegistration, HostSurfaceRegistration, SurfaceResource, SurfaceState,
};
use crate::view::{
    CudaReadView, CudaSurfaceGuard, CudaSurfaceView, CudaTextureGuard, CudaTextureView,
    CudaWriteView,
};

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
    /// Persistent command pool + buffer + completion fence for the
    /// host-pipeline producer copy path
    /// ([`Self::submit_host_copy_image_to_buffer`]). Lazy-init on
    /// first submit; reused with `vkResetCommandPool` after each
    /// submission's fence has signaled. See
    /// [`AdapterPersistentSubmitContext`] for the full contract.
    /// Single-threaded caller convention; multi-threaded callers
    /// serialize through this mutex.
    #[cfg(target_os = "linux")]
    submit_ctx: Mutex<Option<AdapterPersistentSubmitContext>>,
    /// Counts how many times the persistent submit context was
    /// (re)created — used by tests to lock the amortisation invariant
    /// from #620 (steady-state submits must not grow live pools).
    submit_ctx_create_count: AtomicUsize,
}

impl<D: VulkanRhiDevice> CudaSurfaceAdapter<D> {
    /// Construct an empty adapter bound to `device`.
    pub fn new(device: Arc<D>) -> Self {
        Self {
            device,
            surfaces: Registry::new(),
            acquire_timeout: DEFAULT_TIMELINE_WAIT,
            #[cfg(target_os = "linux")]
            submit_ctx: Mutex::new(None),
            submit_ctx_create_count: AtomicUsize::new(0),
        }
    }

    /// Number of times the adapter has materialised its persistent
    /// command pool. Stays at 0 before the first
    /// [`Self::submit_host_copy_image_to_buffer`] call, becomes 1 on
    /// first invocation, and stays at 1 across every subsequent
    /// submit.
    ///
    /// Hidden from the public docs because callers shouldn't depend
    /// on it; tests use it to lock #620's amortisation invariant.
    #[doc(hidden)]
    pub fn submit_pool_create_count(&self) -> usize {
        self.submit_ctx_create_count.load(Ordering::Relaxed)
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
                resource: SurfaceResource::Buffer {
                    pixel_buffer: registration.pixel_buffer,
                },
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

    /// Register an image-flavored surface for the CUDA texture / surface
    /// object path.
    ///
    /// Validates that the texture's format is in the CUDA-mappable
    /// subset (`Rgba8Unorm` / `Rgba16Float` / `Rgba32Float`) at
    /// registration time so the cdylib's later
    /// `cudaExternalMemoryGetMappedMipmappedArray` call doesn't trip on
    /// `CUDA_ERROR_INVALID_VALUE` for an unmappable format. The
    /// host-side allocator (`HostVulkanTexture::new_opaque_fd_export`)
    /// enforces the same check at construction; the adapter's check is
    /// defense in depth and produces a typed
    /// [`AdapterError::BackendRejected`] with a usage-correction hint
    /// for the surface_id.
    ///
    /// Errors:
    /// - [`AdapterError::SurfaceAlreadyRegistered`] — `id` is already
    ///   registered (either flavor).
    /// - [`AdapterError::BackendRejected`] — the texture's format isn't
    ///   in the CUDA-mappable subset.
    pub fn register_host_image_surface(
        &self,
        id: SurfaceId,
        registration: HostImageSurfaceRegistration<D::Privilege>,
    ) -> Result<(), AdapterError> {
        let format = registration.texture.format();
        if !is_cuda_mappable_format(format) {
            return Err(AdapterError::BackendRejected {
                reason: format!(
                    "register_host_image_surface: surface_id={id}: format {format:?} is not \
                     CUDA-mappable; allowed: Rgba8Unorm, Rgba16Float, Rgba32Float \
                     (cudaExternalMemoryGetMappedMipmappedArray accepts only this subset)"
                ),
            });
        }
        let inserted = self.surfaces.register(
            id,
            SurfaceState {
                surface_id: id,
                resource: SurfaceResource::Image {
                    texture: registration.texture,
                },
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

    /// Drop a registered surface (either flavor). Pending guards keep
    /// the underlying `Arc<TimelineSemaphore>` and resource `Arc` alive;
    /// the next acquire returns [`AdapterError::SurfaceNotFound`].
    pub fn unregister_host_surface(&self, id: SurfaceId) -> bool {
        self.surfaces.unregister(id).is_some()
    }

    /// Snapshot the registry size — primarily for tests and
    /// observability.
    pub fn registered_count(&self) -> usize {
        self.surfaces.len()
    }

    /// Power-user accessor: the registered pixel-buffer Arc for a
    /// buffer-flavored surface. Used by the carve-out test in
    /// `streamlib-adapter-cuda-helpers` to call
    /// `export_opaque_fd_memory()` on the underlying buffer; future
    /// cdylib work will route this through the surface-share service
    /// instead. Returns `None` when the surface isn't registered OR
    /// when the surface was registered with an image-flavored
    /// registration (use [`Self::surface_texture`] for those).
    pub fn surface_pixel_buffer(
        &self,
        id: SurfaceId,
    ) -> Option<Arc<<D::Privilege as DevicePrivilege>::Buffer>> {
        self.surfaces
            .with(id, |state| match &state.resource {
                SurfaceResource::Buffer { pixel_buffer } => Some(Arc::clone(pixel_buffer)),
                SurfaceResource::Image { .. } => None,
            })
            .flatten()
    }

    /// Power-user accessor: the registered texture Arc for an
    /// image-flavored surface. Symmetric counterpart to
    /// [`Self::surface_pixel_buffer`]. Returns `None` when the surface
    /// isn't registered OR when the surface was registered with a
    /// buffer-flavored registration.
    pub fn surface_texture(
        &self,
        id: SurfaceId,
    ) -> Option<Arc<<D::Privilege as DevicePrivilege>::Texture>> {
        self.surfaces
            .with(id, |state| match &state.resource {
                SurfaceResource::Image { texture } => Some(Arc::clone(texture)),
                SurfaceResource::Buffer { .. } => None,
            })
            .flatten()
    }

    /// Power-user accessor: the registered timeline-semaphore Arc for
    /// a surface (either flavor). Used by the carve-out test to call
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
        // (vulkan_buffer.rs::new_opaque_fd_export_device_local
        // hardcodes bytes_per_pixel=4 in the example caller). If a
        // future caller uses a non-4-bpp format, extend this check.
        let required_bytes = (image_extent.width as u64)
            .saturating_mul(image_extent.height as u64)
            .saturating_mul(4);

        let session: HostCopySession<D::Privilege> = self
            .surfaces
            .try_begin_write(id, |state| {
                let pixel_buffer = match &state.resource {
                    SurfaceResource::Buffer { pixel_buffer } => pixel_buffer,
                    SurfaceResource::Image { .. } => {
                        return Err(AdapterError::BackendRejected {
                            reason: format!(
                                "submit_host_copy_image_to_buffer: surface_id={id} was \
                                 registered as an image-flavored surface; this host-pipeline \
                                 copy targets a buffer-flavored surface only"
                            ),
                        });
                    }
                };
                let buffer_size = pixel_buffer.size();
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
                    buffer: pixel_buffer.buffer(),
                    signal_value,
                })
            })?
            .ok_or_else(|| AdapterError::WriteContended {
                surface_id: id,
                holder: self.surfaces.describe_contention(id),
            })?;

        if let Err(e) = self.submit_image_to_buffer_copy_signal_timeline(
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
        self.surfaces.try_begin_read(id, |state| match &state.resource {
            SurfaceResource::Buffer { pixel_buffer } => Ok(ReadAcquired {
                timeline: Arc::clone(&state.timeline),
                wait_value: state.current_release_value,
                buffer: pixel_buffer.buffer(),
                size: pixel_buffer.size(),
            }),
            SurfaceResource::Image { .. } => Err(AdapterError::BackendRejected {
                reason: format!(
                    "acquire_read: surface_id={id} was registered as an image-flavored \
                     surface; use acquire_texture (for cudaTextureObject_t) instead"
                ),
            }),
        })
    }

    fn try_begin_write(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteAcquired<D::Privilege>>, AdapterError> {
        let id = surface.id;
        self.surfaces.try_begin_write(id, |state| match &state.resource {
            SurfaceResource::Buffer { pixel_buffer } => Ok(WriteAcquired {
                timeline: Arc::clone(&state.timeline),
                wait_value: state.current_release_value,
                buffer: pixel_buffer.buffer(),
                size: pixel_buffer.size(),
            }),
            SurfaceResource::Image { .. } => Err(AdapterError::BackendRejected {
                reason: format!(
                    "acquire_write: surface_id={id} was registered as an image-flavored \
                     surface; use acquire_surface (for cudaSurfaceObject_t) instead"
                ),
            }),
        })
    }

    fn try_begin_image_read(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ImageReadAcquired<D::Privilege>>, AdapterError> {
        let id = surface.id;
        self.surfaces.try_begin_read(id, |state| match &state.resource {
            SurfaceResource::Image { texture } => {
                let image = texture.image().ok_or_else(|| AdapterError::BackendRejected {
                    reason: format!(
                        "acquire_texture: surface_id={id} texture has no VkImage (placeholder?)"
                    ),
                })?;
                Ok(ImageReadAcquired {
                    timeline: Arc::clone(&state.timeline),
                    wait_value: state.current_release_value,
                    image,
                    width: texture.width(),
                    height: texture.height(),
                    format: texture.format(),
                })
            }
            SurfaceResource::Buffer { .. } => Err(AdapterError::BackendRejected {
                reason: format!(
                    "acquire_texture: surface_id={id} was registered as a buffer-flavored \
                     surface; use acquire_read (for DLPack capsules) instead"
                ),
            }),
        })
    }

    fn try_begin_image_write(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ImageWriteAcquired<D::Privilege>>, AdapterError> {
        let id = surface.id;
        self.surfaces.try_begin_write(id, |state| match &state.resource {
            SurfaceResource::Image { texture } => {
                let image = texture.image().ok_or_else(|| AdapterError::BackendRejected {
                    reason: format!(
                        "acquire_surface: surface_id={id} texture has no VkImage (placeholder?)"
                    ),
                })?;
                Ok(ImageWriteAcquired {
                    timeline: Arc::clone(&state.timeline),
                    wait_value: state.current_release_value,
                    image,
                    width: texture.width(),
                    height: texture.height(),
                    format: texture.format(),
                })
            }
            SurfaceResource::Buffer { .. } => Err(AdapterError::BackendRejected {
                reason: format!(
                    "acquire_surface: surface_id={id} was registered as a buffer-flavored \
                     surface; use acquire_write (for DLPack capsules) instead"
                ),
            }),
        })
    }

    fn finalize_image_read(
        &self,
        surface_id: SurfaceId,
        acquired: ImageReadAcquired<D::Privilege>,
    ) -> Result<CudaTextureView<'_>, AdapterError> {
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
        Ok(CudaTextureView {
            image: acquired.image,
            width: acquired.width,
            height: acquired.height,
            format: acquired.format,
            _marker: PhantomData,
        })
    }

    fn finalize_image_write(
        &self,
        surface_id: SurfaceId,
        acquired: ImageWriteAcquired<D::Privilege>,
    ) -> Result<CudaSurfaceView<'_>, AdapterError> {
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
        Ok(CudaSurfaceView {
            image: acquired.image,
            width: acquired.width,
            height: acquired.height,
            format: acquired.format,
            _marker: PhantomData,
        })
    }

    /// Blocking acquire of read-only image access — the
    /// `cudaTextureObject_t` side of CUDA's texture interop.
    /// Returns a [`CudaTextureGuard`] scoped to the acquire window;
    /// drop releases the read holder and signals the timeline.
    pub fn acquire_texture<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<CudaTextureGuard<'g, D>, AdapterError> {
        let acquired = match self.try_begin_image_read(surface)? {
            Some(a) => a,
            None => {
                return Err(AdapterError::WriteContended {
                    surface_id: surface.id,
                    holder: "writer".to_string(),
                });
            }
        };
        let view = self.finalize_image_read(surface.id, acquired)?;
        Ok(CudaTextureGuard {
            adapter: self,
            surface_id: surface.id,
            view,
        })
    }

    /// Non-blocking variant of [`Self::acquire_texture`]. Returns
    /// `Ok(None)` on contention rather than blocking.
    pub fn try_acquire_texture<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<CudaTextureGuard<'g, D>>, AdapterError> {
        let acquired = match self.try_begin_image_read(surface)? {
            Some(a) => a,
            None => return Ok(None),
        };
        let view = self.finalize_image_read(surface.id, acquired)?;
        Ok(Some(CudaTextureGuard {
            adapter: self,
            surface_id: surface.id,
            view,
        }))
    }

    /// Blocking acquire of read-write image access — the
    /// `cudaSurfaceObject_t` side of CUDA's texture interop. Returns
    /// a [`CudaSurfaceGuard`] scoped to the acquire window; drop
    /// releases the write hold and signals the timeline.
    pub fn acquire_surface<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<CudaSurfaceGuard<'g, D>, AdapterError> {
        let acquired = match self.try_begin_image_write(surface)? {
            Some(a) => a,
            None => {
                return Err(AdapterError::WriteContended {
                    surface_id: surface.id,
                    holder: self.surfaces.describe_contention(surface.id),
                });
            }
        };
        let view = self.finalize_image_write(surface.id, acquired)?;
        Ok(CudaSurfaceGuard {
            adapter: self,
            surface_id: surface.id,
            view,
        })
    }

    /// Non-blocking variant of [`Self::acquire_surface`].
    pub fn try_acquire_surface<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<CudaSurfaceGuard<'g, D>>, AdapterError> {
        let acquired = match self.try_begin_image_write(surface)? {
            Some(a) => a,
            None => return Ok(None),
        };
        let view = self.finalize_image_write(surface.id, acquired)?;
        Ok(Some(CudaSurfaceGuard {
            adapter: self,
            surface_id: surface.id,
            view,
        }))
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

/// Records and submits the host-pipeline producer copy
/// (`vkCmdCopyImageToBuffer`) using the adapter's persistent command
/// pool ([`AdapterPersistentSubmitContext`]). Transitions `image`
/// into `TRANSFER_SRC_OPTIMAL`, copies its full color plane into
/// `buffer`, transitions back to `image_layout`, and submits with a
/// GPU-side timeline signal at `signal_value`.
///
/// The post-copy buffer barrier is intentionally absent: cuda imports
/// the buffer's memory once at registration and reads it through
/// `cudaWaitExternalSemaphore` on the same timeline, not via host
/// `vkMapMemory`, so a HOST_READ barrier would be both unnecessary
/// and wrong.
#[cfg(target_os = "linux")]
impl<D> CudaSurfaceAdapter<D>
where
    D: VulkanRhiDevice,
{
    fn submit_image_to_buffer_copy_signal_timeline(
        &self,
        image: vk::Image,
        image_layout: vk::ImageLayout,
        buffer: vk::Buffer,
        image_extent: vk::Extent3D,
        timeline: &<D::Privilege as DevicePrivilege>::TimelineSemaphore,
        signal_value: u64,
    ) -> Result<(), AdapterError> {
        let vk_device = self.device.device();
        let queue = self.device.queue();
        let qf = self.device.queue_family_index();
        let transfer_layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;

        let mut guard = self
            .submit_ctx
            .lock()
            .map_err(|_| AdapterError::BackendRejected {
                reason:
                    "submit_image_to_buffer_copy_signal_timeline: persistent submit context mutex poisoned"
                        .into(),
            })?;
        if guard.is_none() {
            *guard = Some(AdapterPersistentSubmitContext::new(vk_device, qf)?);
            self.submit_ctx_create_count.fetch_add(1, Ordering::Relaxed);
        }
        let submit_ctx = guard.as_ref().expect("submit_ctx populated above");
        let cmd = submit_ctx.cmd;

        submit_ctx.reset_for_recording(vk_device)?;

        let begin_info = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();
        unsafe { vk_device.begin_command_buffer(cmd, &begin_info) }.map_err(|e| {
            AdapterError::BackendRejected {
                reason: format!("begin_command_buffer: {e}"),
            }
        })?;

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
            vk_device.cmd_copy_image_to_buffer(
                cmd,
                image,
                transfer_layout,
                buffer,
                &[copy_region],
            );
        }

        let post_barrier = build_color_image_barrier(image, qf, transfer_layout, image_layout);
        let post_barriers = [post_barrier];
        let post_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&post_barriers)
            .build();
        unsafe { vk_device.cmd_pipeline_barrier2(cmd, &post_dep) };

        unsafe { vk_device.end_command_buffer(cmd) }.map_err(|e| {
            AdapterError::BackendRejected {
                reason: format!("end_command_buffer: {e}"),
            }
        })?;

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

        unsafe { self.device.submit_to_queue(queue, &[submit], submit_ctx.fence) }.map_err(
            |e| AdapterError::BackendRejected {
                reason: format!("submit_to_queue: {e}"),
            },
        )?;

        Ok(())
    }
}

/// Persistent per-adapter command pool, command buffer, and
/// completion fence — replaces the create-and-destroy-per-submit
/// pattern that used to churn `vkCreateCommandPool` /
/// `vkDestroyCommandPool` on every host-pipeline copy. Same shape
/// lives in
/// `streamlib-adapter-cpu-readback::adapter::AdapterPersistentSubmitContext`
/// and `streamlib-adapter-vulkan::adapter::AdapterPersistentSubmitContext`;
/// fix ALL THREE if you change ANY (issue #620 + #640 AI Agent
/// Notes — `streamlib-adapter-abi` deliberately does not depend on
/// `vulkanalia`, so duplication is the project pattern here).
///
/// The fence is created signaled so the first submit doesn't block.
/// Subsequent submits wait on it (instant when the prior submit has
/// already drained, which is the steady state). `vkResetCommandPool`
/// is the cheap path per Vulkan spec — recycles every command
/// buffer's memory in one call.
#[cfg(target_os = "linux")]
struct AdapterPersistentSubmitContext {
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
}

#[cfg(target_os = "linux")]
impl AdapterPersistentSubmitContext {
    fn new(device: &vulkanalia::Device, qf: u32) -> Result<Self, AdapterError> {
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(qf)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT)
            .build();
        let pool =
            unsafe { device.create_command_pool(&pool_info, None) }.map_err(|e| {
                AdapterError::BackendRejected {
                    reason: format!("create_command_pool: {e}"),
                }
            })?;

        let alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1)
            .build();
        let cmd = match unsafe { device.allocate_command_buffers(&alloc_info) } {
            Ok(v) => v[0],
            Err(e) => {
                unsafe { device.destroy_command_pool(pool, None) };
                return Err(AdapterError::BackendRejected {
                    reason: format!("allocate_command_buffers: {e}"),
                });
            }
        };

        let fence_info = vk::FenceCreateInfo::builder()
            .flags(vk::FenceCreateFlags::SIGNALED)
            .build();
        let fence = match unsafe { device.create_fence(&fence_info, None) } {
            Ok(f) => f,
            Err(e) => {
                unsafe { device.destroy_command_pool(pool, None) };
                return Err(AdapterError::BackendRejected {
                    reason: format!("create_fence: {e}"),
                });
            }
        };

        Ok(Self { pool, cmd, fence })
    }

    /// Wait for the previous submit's fence, reset it, then reset the
    /// command pool so the single command buffer is ready to be
    /// re-recorded. Steady-state cost is the wait, which is instant
    /// when the prior submit has already drained.
    fn reset_for_recording(&self, device: &vulkanalia::Device) -> Result<(), AdapterError> {
        unsafe { device.wait_for_fences(&[self.fence], true, u64::MAX) }.map_err(|e| {
            AdapterError::BackendRejected {
                reason: format!("wait_for_fences (persistent submit fence): {e}"),
            }
        })?;
        unsafe { device.reset_fences(&[self.fence]) }.map_err(|e| {
            AdapterError::BackendRejected {
                reason: format!("reset_fences (persistent submit fence): {e}"),
            }
        })?;
        unsafe { device.reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty()) }
            .map_err(|e| AdapterError::BackendRejected {
                reason: format!("reset_command_pool (persistent submit pool): {e}"),
            })?;
        Ok(())
    }

    /// Tear down the pool + fence. Waits on the fence first to drain
    /// any pending GPU work so destruction is safe.
    fn destroy(self, device: &vulkanalia::Device) {
        let _ = unsafe { device.wait_for_fences(&[self.fence], true, u64::MAX) };
        unsafe {
            device.destroy_fence(self.fence, None);
            device.destroy_command_pool(self.pool, None);
        }
    }
}

#[cfg(target_os = "linux")]
impl<D: VulkanRhiDevice> Drop for CudaSurfaceAdapter<D> {
    fn drop(&mut self) {
        let mut guard = match self.submit_ctx.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(ctx) = guard.take() {
            ctx.destroy(self.device.device());
        }
    }
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

/// Image-flavored read snapshot — sibling of [`ReadAcquired`] for the
/// `acquire_texture` path. Carries the `vk::Image` handle + the
/// dimensions / format the cdylib needs to build a
/// `cudaTextureObject_t`.
struct ImageReadAcquired<P: DevicePrivilege> {
    timeline: Arc<P::TimelineSemaphore>,
    wait_value: u64,
    image: vk::Image,
    width: u32,
    height: u32,
    format: TextureFormat,
}

/// Image-flavored write snapshot — sibling of [`WriteAcquired`] for
/// the `acquire_surface` path.
struct ImageWriteAcquired<P: DevicePrivilege> {
    timeline: Arc<P::TimelineSemaphore>,
    wait_value: u64,
    image: vk::Image,
    width: u32,
    height: u32,
    format: TextureFormat,
}

/// Format gate enforced at [`CudaSurfaceAdapter::register_host_image_surface`]
/// time. The CUDA-mappable subset is fixed by
/// `cudaExternalMemoryGetMappedMipmappedArray`'s accepted
/// `cudaChannelFormatDesc`: 1/2/4-channel `R8/R16/R32` integer or
/// 16/32-bit float, no sRGB, no `BGR*`, no three-channel formats.
/// Mapped to `TextureFormat` variants the streamlib RHI exposes today:
/// `Rgba8Unorm`, `Rgba16Float`, `Rgba32Float`. Other variants
/// (`Rgba8UnormSrgb`, `Bgra8Unorm`, `Bgra8UnormSrgb`, `Nv12`) are
/// rejected.
fn is_cuda_mappable_format(format: TextureFormat) -> bool {
    matches!(
        format,
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba16Float | TextureFormat::Rgba32Float
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Locks the CUDA-mappable format gate. Mentally revert
    /// `is_cuda_mappable_format` to `|_| true` (or remove the
    /// `matches!` arm) and the second assertion fires.
    #[test]
    fn cuda_mappable_format_accepts_supported_subset() {
        assert!(is_cuda_mappable_format(TextureFormat::Rgba8Unorm));
        assert!(is_cuda_mappable_format(TextureFormat::Rgba16Float));
        assert!(is_cuda_mappable_format(TextureFormat::Rgba32Float));
    }

    #[test]
    fn cuda_mappable_format_rejects_other_variants() {
        assert!(!is_cuda_mappable_format(TextureFormat::Rgba8UnormSrgb));
        assert!(!is_cuda_mappable_format(TextureFormat::Bgra8Unorm));
        assert!(!is_cuda_mappable_format(TextureFormat::Bgra8UnormSrgb));
        assert!(!is_cuda_mappable_format(TextureFormat::Nv12));
    }
}
