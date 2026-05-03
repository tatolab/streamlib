// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `CpuReadbackSurfaceAdapter<D>` — generic over device flavor.
//!
//! The adapter holds a registry of pre-registered surfaces and a
//! [`CpuReadbackCopyTrigger`] that knows how to schedule the GPU copy
//! between the source `VkImage` and the per-plane staging
//! `VkBuffer`s. Two trigger flavors ship in this crate:
//!
//! - [`InProcessCpuReadbackCopyTrigger`] — generic over any
//!   `D: VulkanRhiDevice`. Records `vkCmdCopyImageToBuffer` /
//!   `vkCmdCopyBufferToImage` and submits via `D::submit_to_queue`,
//!   signaling the surface's timeline at end-of-submit. Used by
//!   in-process Rust callers that hold a host-flavor device. Returns
//!   an error if invoked against a surface with no source image
//!   (e.g. a consumer-flavor adapter whose registration didn't
//!   import the host's image — that's nonsensical for cpu-readback
//!   since the consumer can't reach the host's `VkImage`).
//!
//! - [`EscalateCpuReadbackCopyTrigger`] (typically constructed in a
//!   subprocess cdylib using its existing escalate stdin/stdout
//!   pipe) — sends a `run_cpu_readback_copy` IPC request to the host
//!   and parses the timeline value from the response.
//!
//! The adapter itself is fully generic; the privilege-flavor split
//! is entirely in the trigger choice. The cdylib's dep graph
//! excludes `streamlib`, so `HostVulkanDevice` is not reachable from
//! a cdylib — the wrong-way (constructing an in-process trigger
//! against a host device from inside a subprocess) is impossible by
//! the dep graph alone.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use streamlib_consumer_rhi::{
    DevicePrivilege, VulkanPixelBufferLike, VulkanRhiDevice, VulkanTextureLike,
    VulkanTimelineSemaphoreLike,
};
use streamlib_adapter_abi::{
    AdapterError, ReadGuard, Registry, StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceId,
    SurfaceRegistration, WriteGuard,
};
use tracing::instrument;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use streamlib_consumer_rhi::VulkanLayout;

use crate::state::{HostSurfaceRegistration, PlaneSlot, SurfaceState};
use crate::view::{
    CpuReadbackPlaneView, CpuReadbackPlaneViewMut, CpuReadbackReadView, CpuReadbackWriteView,
};

/// Default per-acquire wait timeout. Bounds the prior-work timeline
/// wait, the trigger call, and the post-copy timeline wait.
const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-acquire trigger context — everything the trigger needs to
/// either record + submit a Vulkan copy (in-process flavor) or
/// dispatch an IPC trigger (subprocess flavor).
///
/// The adapter owns the `Arc`s; this borrows them for the duration
/// of one trigger call. The trigger MUST NOT clone-into-storage —
/// the per-surface state may be torn down on `unregister_host_surface`.
pub struct CpuReadbackTriggerContext<'a, P: DevicePrivilege> {
    /// Surface id the consumer addressed. Subprocess triggers thread
    /// this through the IPC; in-process triggers use it for tracing.
    pub surface_id: SurfaceId,
    /// Source `VkImage` if the registration provided one. Always
    /// present for host-flavor registrations; consumer-flavor
    /// registrations don't import the source image (it lives on the
    /// host device, unreachable from the consumer device), so this
    /// is `None` for consumer-flavor triggers — they ignore it and
    /// use the IPC payload.
    pub image: Option<vk::Image>,
    /// Layout the source image is currently in. Same caveat —
    /// only meaningful for host-flavor triggers.
    pub from_layout: vk::ImageLayout,
    /// Pixel format. Drives the per-plane aspect masks and copy
    /// region geometry on the host side.
    pub format: SurfaceFormat,
    /// Shared timeline semaphore (host-allocated as exportable;
    /// consumer holds the imported handle). Both trigger flavors
    /// signal a new value (host trigger via submit; subprocess
    /// trigger via IPC) so the adapter's wait sees it.
    pub timeline: &'a Arc<P::TimelineSemaphore>,
    /// Per-plane staging buffer info. Trigger reads `buffer` and
    /// the geometry; mapped pointers are reached by the adapter
    /// when building the post-copy view.
    pub planes: &'a [TriggerPlane],
    /// Queue family the host side should use for any pipeline
    /// barriers. Set from `D::queue_family_index()` at the
    /// snapshot site.
    pub queue_family_index: u32,
    /// Suggested next timeline value to signal. The trigger MAY
    /// signal exactly this value (in-process flavor — needs to
    /// know which value to pass to `VkSemaphoreSubmitInfo`) or
    /// MAY return a different value (subprocess flavor — the
    /// host side decides; consumer waits on whatever it returns).
    pub suggested_signal_value: u64,
}

/// Per-plane info passed to a trigger. Tightly-packed staging
/// buffer geometry plus the raw `vk::Buffer` handle. The trigger
/// reads `buffer` (and on the host side records copy regions
/// against it); the adapter reads the matching mapped pointer
/// when assembling the customer-facing view.
#[derive(Clone, Copy)]
pub struct TriggerPlane {
    pub buffer: vk::Buffer,
    pub width: u32,
    pub height: u32,
    pub bytes_per_pixel: u32,
}

/// Trigger interface implemented per privilege flavor. The adapter
/// holds an `Arc<dyn CpuReadbackCopyTrigger<D::Privilege>>` and
/// dispatches to it on every acquire (`run_copy_image_to_buffer`)
/// and every write release (`run_copy_buffer_to_image`).
///
/// Returns the timeline value the consumer should wait on. The
/// in-process trigger signals exactly `ctx.suggested_signal_value`
/// in its submit and returns it; the subprocess trigger forwards
/// the surface id over IPC, parses the host's response, and
/// returns whatever value the host reports.
pub trait CpuReadbackCopyTrigger<P: DevicePrivilege>: Send + Sync {
    fn run_copy_image_to_buffer(
        &self,
        ctx: &CpuReadbackTriggerContext<'_, P>,
    ) -> Result<u64, AdapterError>;

    fn run_copy_buffer_to_image(
        &self,
        ctx: &CpuReadbackTriggerContext<'_, P>,
    ) -> Result<u64, AdapterError>;
}

/// CPU-readback `SurfaceAdapter`, generic over device flavor.
///
/// Construct with the appropriate trigger:
/// - In-process Rust caller (host flavor): `CpuReadbackSurfaceAdapter::new(host_device, Arc::new(InProcessCpuReadbackCopyTrigger::new(host_device)))`.
/// - Subprocess cdylib (consumer flavor): `CpuReadbackSurfaceAdapter::new(consumer_device, Arc::new(EscalateTrigger::new(escalate_pipe)))`.
pub struct CpuReadbackSurfaceAdapter<D: VulkanRhiDevice> {
    device: Arc<D>,
    surfaces: Registry<SurfaceState<D::Privilege>>,
    acquire_timeout: Duration,
    trigger: Arc<dyn CpuReadbackCopyTrigger<D::Privilege>>,
}

impl<D: VulkanRhiDevice + 'static> CpuReadbackSurfaceAdapter<D> {
    /// Construct an empty adapter bound to `device` with `trigger` as
    /// the per-acquire dispatch mechanism.
    pub fn new(device: Arc<D>, trigger: Arc<dyn CpuReadbackCopyTrigger<D::Privilege>>) -> Self {
        Self {
            device,
            surfaces: Registry::new(),
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
            trigger,
        }
    }

    /// Override the per-acquire wait timeout. Default 5 s.
    pub fn with_acquire_timeout(mut self, timeout: Duration) -> Self {
        self.acquire_timeout = timeout;
        self
    }

    /// Returns the underlying device.
    pub fn device(&self) -> &Arc<D> {
        &self.device
    }

    /// Register a pre-allocated (host) or pre-imported (consumer)
    /// surface with this adapter.
    #[instrument(level = "debug", skip(self, registration), fields(surface_id = id))]
    pub fn register_host_surface(
        &self,
        id: SurfaceId,
        registration: HostSurfaceRegistration<D::Privilege>,
    ) -> Result<(), AdapterError> {
        let format = registration.format;
        let width = registration.width;
        let height = registration.height;
        let plane_count = format.plane_count() as usize;

        if registration.staging_planes.len() != plane_count {
            return Err(AdapterError::UnsupportedFormat {
                surface_id: id,
                reason: format!(
                    "{format:?} requires {plane_count} staging plane(s), got {}",
                    registration.staging_planes.len()
                ),
            });
        }

        // Validate dimensions are compatible with the format's chroma
        // subsampling. NV12's UV plane is half-resolution; odd sizes
        // would silently lose the trailing column / row.
        if format.plane_count() > 1 && (width % 2 != 0 || height % 2 != 0) {
            return Err(AdapterError::UnsupportedFormat {
                surface_id: id,
                reason: format!(
                    "{format:?} requires even surface dimensions for chroma subsampling, got {width}x{height}"
                ),
            });
        }

        let mut planes = Vec::with_capacity(plane_count);
        for (plane_idx, staging) in registration.staging_planes.into_iter().enumerate() {
            let pw = format.plane_width(width, plane_idx as u32);
            let ph = format.plane_height(height, plane_idx as u32);
            let pbpp = format.plane_bytes_per_pixel(plane_idx as u32);

            // The staging buffer's recorded geometry must match the
            // plane's logical geometry. Caller's responsibility to
            // size them correctly — we surface a clear error if not.
            if staging.width() != pw
                || staging.height() != ph
                || staging.bytes_per_pixel() != pbpp
            {
                return Err(AdapterError::UnsupportedFormat {
                    surface_id: id,
                    reason: format!(
                        "plane {} staging geometry mismatch: expected {pw}x{ph}@{pbpp}bpp, got {}x{}@{}bpp",
                        plane_idx,
                        staging.width(),
                        staging.height(),
                        staging.bytes_per_pixel()
                    ),
                });
            }

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
            current_layout: registration.initial_image_layout,
            read_holders: 0,
            write_held: false,
            current_release_value: 0,
            format,
            width,
            height,
        };
        if !self.surfaces.register(id, state) {
            return Err(AdapterError::SurfaceAlreadyRegistered { surface_id: id });
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

    fn snapshot_for_acquire(
        state: &mut SurfaceState<D::Privilege>,
    ) -> AcquireSnapshot<D::Privilege> {
        let timeline = Arc::clone(&state.timeline);
        let wait_value = state.current_release_value;
        let image = state.texture.as_ref().and_then(|t| t.image());
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
        AcquireSnapshot {
            timeline,
            wait_value,
            image,
            from,
            format,
            width,
            height,
            planes: plane_snaps,
            _marker: PhantomData,
        }
    }

    fn try_begin_read_inner(
        &self,
        surface_id: SurfaceId,
    ) -> Result<Option<AcquireSnapshot<D::Privilege>>, AdapterError> {
        self.surfaces
            .try_begin_read(surface_id, |state| Ok(Self::snapshot_for_acquire(state)))
    }

    fn try_begin_write_inner(
        &self,
        surface_id: SurfaceId,
    ) -> Result<Option<AcquireSnapshot<D::Privilege>>, AdapterError> {
        self.surfaces
            .try_begin_write(surface_id, |state| Ok(Self::snapshot_for_acquire(state)))
    }

    fn rollback_acquire(&self, surface_id: SurfaceId, write: bool) {
        if write {
            self.surfaces.rollback_write(surface_id);
        } else {
            self.surfaces.rollback_read(surface_id);
        }
    }

    /// Build the trigger's per-acquire context from a snapshot.
    fn make_trigger_context<'a>(
        &self,
        surface_id: SurfaceId,
        snap: &'a AcquireSnapshot<D::Privilege>,
        suggested_signal_value: u64,
        trigger_planes: &'a [TriggerPlane],
    ) -> CpuReadbackTriggerContext<'a, D::Privilege> {
        CpuReadbackTriggerContext {
            surface_id,
            image: snap.image,
            from_layout: snap.from.as_vk(),
            format: snap.format,
            timeline: &snap.timeline,
            planes: trigger_planes,
            queue_family_index: self.device.queue_family_index(),
            suggested_signal_value,
        }
    }

    fn log_acquire(
        &self,
        surface_id: SurfaceId,
        snap: &AcquireSnapshot<D::Privilege>,
        write: bool,
    ) {
        let total_bytes: u64 = snap.planes.iter().map(|p| p.byte_size).sum();
        tracing::info!(
            surface_id = surface_id,
            width = snap.width,
            height = snap.height,
            format = ?snap.format,
            plane_count = snap.planes.len(),
            bytes = total_bytes,
            mode = if write { "write" } else { "read" },
            "cpu-readback: GPU↔CPU copy of {}x{} {:?} surface, {} bytes total ({} planes)",
            snap.width,
            snap.height,
            snap.format,
            total_bytes,
            snap.planes.len(),
        );
    }

    /// Bridge entry: run `vkCmdCopyImageToBuffer` for `surface_id` on
    /// the host's queue without going through the in-process
    /// `try_begin_*` / `end_*_access` registry counters.
    ///
    /// Used by the host-side `CpuReadbackBridge` impl that the
    /// escalate handler reaches when a subprocess sends
    /// `run_cpu_readback_copy(direction=image_to_buffer)`. The
    /// subprocess's own consumer-flavor adapter manages contention
    /// on its side; the host bridge call is stateless from a
    /// counter-tracking perspective. **v1 limitation**: do not mix
    /// in-process host `acquire_*` and subprocess bridge calls
    /// against the same surface concurrently — the registry's
    /// counters won't observe the bridge call.
    pub fn run_bridge_copy_image_to_buffer(
        &self,
        surface_id: SurfaceId,
    ) -> Result<u64, AdapterError> {
        self.run_bridge_copy_inner(surface_id, BridgeDirection::ImageToBuffer)
    }

    /// Bridge entry: run `vkCmdCopyBufferToImage` for `surface_id`.
    /// Mirror of [`Self::run_bridge_copy_image_to_buffer`] — same
    /// semantics, opposite direction. Used on subprocess write
    /// release.
    pub fn run_bridge_copy_buffer_to_image(
        &self,
        surface_id: SurfaceId,
    ) -> Result<u64, AdapterError> {
        self.run_bridge_copy_inner(surface_id, BridgeDirection::BufferToImage)
    }

    fn run_bridge_copy_inner(
        &self,
        surface_id: SurfaceId,
        direction: BridgeDirection,
    ) -> Result<u64, AdapterError> {
        let snap = self
            .surfaces
            .with_mut(surface_id, |state| Self::snapshot_for_acquire(state))
            .ok_or(AdapterError::SurfaceNotFound { surface_id })?;
        let next_value = snap.wait_value + 1;
        let trigger_planes: Vec<TriggerPlane> = snap
            .planes
            .iter()
            .map(|p| TriggerPlane {
                buffer: p.buffer,
                width: p.width,
                height: p.height,
                bytes_per_pixel: p.bytes_per_pixel,
            })
            .collect();
        let ctx = self.make_trigger_context(surface_id, &snap, next_value, &trigger_planes);
        let signaled = match direction {
            BridgeDirection::ImageToBuffer => self.trigger.run_copy_image_to_buffer(&ctx)?,
            BridgeDirection::BufferToImage => self.trigger.run_copy_buffer_to_image(&ctx)?,
        };
        self.surfaces.with_mut(surface_id, |state| {
            state.current_release_value = signaled;
            state.current_layout = VulkanLayout::GENERAL;
        });
        Ok(signaled)
    }

    fn acquire_inner(
        &self,
        surface_id: SurfaceId,
        write: bool,
        blocking: bool,
    ) -> Result<Option<AcquireOutcome<D::Privilege>>, AdapterError> {
        let snap = match if write {
            self.try_begin_write_inner(surface_id)?
        } else {
            self.try_begin_read_inner(surface_id)?
        } {
            Some(s) => s,
            None => {
                return if blocking {
                    Err(AdapterError::WriteContended {
                        surface_id,
                        holder: if write {
                            self.surfaces.describe_contention(surface_id)
                        } else {
                            "writer".to_string()
                        },
                    })
                } else {
                    Ok(None)
                };
            }
        };
        self.log_acquire(surface_id, &snap, write);

        // Wait for prior work to drain (last release-value the host
        // signaled). Skipped on the first acquire since wait_value=0
        // and a fresh timeline is already at counter 0.
        if snap.wait_value > 0
            && snap
                .timeline
                .wait(snap.wait_value, self.acquire_timeout.as_nanos() as u64)
                .is_err()
        {
            self.rollback_acquire(surface_id, write);
            return Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            });
        }

        let next_value = snap.wait_value + 1;
        let trigger_planes: Vec<TriggerPlane> = snap
            .planes
            .iter()
            .map(|p| TriggerPlane {
                buffer: p.buffer,
                width: p.width,
                height: p.height,
                bytes_per_pixel: p.bytes_per_pixel,
            })
            .collect();
        let ctx = self.make_trigger_context(surface_id, &snap, next_value, &trigger_planes);

        let signaled = match self.trigger.run_copy_image_to_buffer(&ctx) {
            Ok(v) => v,
            Err(e) => {
                self.rollback_acquire(surface_id, write);
                return Err(e);
            }
        };

        // Wait for the trigger's signaled value.
        if snap
            .timeline
            .wait(signaled, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            self.rollback_acquire(surface_id, write);
            return Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            });
        }
        self.surfaces.with_mut(surface_id, |state| {
            state.current_layout = VulkanLayout::GENERAL;
            state.current_release_value = signaled;
        });

        Ok(Some(AcquireOutcome { snap }))
    }
}

#[cfg(target_os = "linux")]
impl<D: VulkanRhiDevice + 'static> SurfaceAdapter for CpuReadbackSurfaceAdapter<D> {
    type ReadView<'g> = CpuReadbackReadView<'g>;
    type WriteView<'g> = CpuReadbackWriteView<'g>;

    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        let outcome = self
            .acquire_inner(surface.id, false, true)?
            .expect("blocking acquire returned None");
        Ok(ReadGuard::new(self, surface.id, build_read_view(&outcome.snap)))
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        let outcome = self
            .acquire_inner(surface.id, true, true)?
            .expect("blocking acquire returned None");
        Ok(WriteGuard::new(
            self,
            surface.id,
            build_write_view(&outcome.snap),
        ))
    }

    fn try_acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError> {
        match self.acquire_inner(surface.id, false, false)? {
            Some(o) => Ok(Some(ReadGuard::new(self, surface.id, build_read_view(&o.snap)))),
            None => Ok(None),
        }
    }

    fn try_acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError> {
        match self.acquire_inner(surface.id, true, false)? {
            Some(o) => Ok(Some(WriteGuard::new(
                self,
                surface.id,
                build_write_view(&o.snap),
            ))),
            None => Ok(None),
        }
    }

    fn end_read_access(&self, surface_id: SurfaceId) {
        // Read release just decrements the holder counter — no GPU
        // work to flush, and no `signal_host` to issue. The timeline
        // is already at `current_release_value` from the trigger
        // that ran on acquire; future acquires read that same value
        // as their `wait_value` and pass through immediately.
        //
        // Pre-#562 the consumer-flavor adapter would also call
        // `timeline.signal_host(current + 1)` here. That works for
        // the host-only in-process case but is unsound cross-process:
        // the host adapter advances the shared timeline through its
        // trigger's `vkQueueSubmit2` signal, the consumer adapter
        // tracks an INDEPENDENT local counter, and a host-side
        // `vkSignalSemaphore` from the consumer flavor races the
        // host's queue-submit signals against the same kernel object —
        // monotonically-increasing values aren't preserved across the
        // two writers, tripping VUID-VkSemaphoreSignalInfo-value-03259.
        // Dropping the call is also a no-op for the host case
        // because the trigger already covered the timeline advance.
        let outcome = self.surfaces.with_mut(surface_id, |state| {
            debug_assert!(state.read_holders > 0, "read release without acquire");
            state.dec_read_holders();
        });
        if outcome.is_none() {
            tracing::warn!(?surface_id, "end_read_access on unknown surface");
        }
    }

    fn end_write_access(&self, surface_id: SurfaceId) {
        // Snapshot the work to do under the lock, then run the
        // trigger unlocked.
        let snap = self.surfaces.with_mut(surface_id, |state| {
            debug_assert!(state.write_held, "write release without acquire");
            let timeline = Arc::clone(&state.timeline);
            let wait_value = state.current_release_value;
            let image = state.texture.as_ref().and_then(|t| t.image());
            let from = state.current_layout;
            let format = state.format;
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
            AcquireSnapshot {
                timeline,
                wait_value,
                image,
                from,
                format,
                width: state.width,
                height: state.height,
                planes: plane_snaps,
                _marker: PhantomData,
            }
        });
        let snap = match snap {
            Some(s) => s,
            None => {
                tracing::warn!(?surface_id, "end_write_access on unknown surface");
                return;
            }
        };

        let next_value = snap.wait_value + 1;
        let trigger_planes: Vec<TriggerPlane> = snap
            .planes
            .iter()
            .map(|p| TriggerPlane {
                buffer: p.buffer,
                width: p.width,
                height: p.height,
                bytes_per_pixel: p.bytes_per_pixel,
            })
            .collect();
        let ctx = self.make_trigger_context(surface_id, &snap, next_value, &trigger_planes);

        let signaled = match self.trigger.run_copy_buffer_to_image(&ctx) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(?surface_id, error = %e, "cpu-readback flush trigger failed");
                self.surfaces.rollback_write(surface_id);
                return;
            }
        };
        if snap
            .timeline
            .wait(signaled, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            tracing::error!(?surface_id, "timeline wait timed out on write flush");
            self.surfaces.rollback_write(surface_id);
            return;
        }

        self.surfaces.with_mut(surface_id, |state| {
            state.set_write_held(false);
            state.current_layout = VulkanLayout::GENERAL;
            state.current_release_value = signaled;
        });
    }
}

// =====================================================================
// In-process trigger — generic over `D: VulkanRhiDevice`.
// =====================================================================

/// In-process [`CpuReadbackCopyTrigger`] that records `vkCmdCopy*`
/// against the device and submits via `D::submit_to_queue`. Generic
/// over any `D: VulkanRhiDevice` — works against either flavor of the
/// device, but only meaningful for host-flavor (the consumer's
/// `VkDevice` cannot reach a `VkImage` allocated on the host's
/// device, so the trigger errors when invoked with `image: None`).
///
/// Cdylibs use [`crate::EscalateCpuReadbackCopyTrigger`] (or their
/// own trigger that talks to the host over IPC) instead — the
/// in-process trigger is reachable to them only against
/// `ConsumerVulkanDevice`, which fails at the `image.is_some()`
/// check.
///
/// Holds a single persistent `vk::CommandPool` + command buffer +
/// completion fence ([`AdapterPersistentSubmitContext`]), reset and
/// reused on every submit. The pool is lazy-initialised on the first
/// `run_copy_*` call so `new()` stays infallible. Single-threaded
/// caller convention; future concurrent callers serialise through
/// the inner [`Mutex`] (correct, just less concurrent than
/// thread-local pools — see issue #620 AI Agent Notes).
pub struct InProcessCpuReadbackCopyTrigger<D: VulkanRhiDevice> {
    device: Arc<D>,
    #[cfg(target_os = "linux")]
    submit_ctx: Mutex<Option<AdapterPersistentSubmitContext>>,
    /// Counts the number of times the persistent submit context was
    /// (re)created — incremented on lazy-init and on rebuild after
    /// device loss. Tests assert this stays at 1 after N submits to
    /// lock the amortisation contract from #620.
    submit_ctx_create_count: AtomicUsize,
}

impl<D: VulkanRhiDevice> InProcessCpuReadbackCopyTrigger<D> {
    pub fn new(device: Arc<D>) -> Self {
        Self {
            device,
            #[cfg(target_os = "linux")]
            submit_ctx: Mutex::new(None),
            submit_ctx_create_count: AtomicUsize::new(0),
        }
    }

    /// Number of times this trigger has materialised its persistent
    /// command pool. Stays at 0 before the first submit, becomes 1
    /// after the first submit, and stays at 1 across every subsequent
    /// submit unless the pool is rebuilt (which never happens today
    /// — driver-loss recovery would bump it).
    ///
    /// Hidden from the public docs because callers shouldn't depend
    /// on it; tests use it to lock #620's amortisation invariant.
    #[doc(hidden)]
    pub fn submit_pool_create_count(&self) -> usize {
        self.submit_ctx_create_count.load(Ordering::Relaxed)
    }
}

#[cfg(target_os = "linux")]
impl<D: VulkanRhiDevice + 'static> CpuReadbackCopyTrigger<D::Privilege>
    for InProcessCpuReadbackCopyTrigger<D>
{
    fn run_copy_image_to_buffer(
        &self,
        ctx: &CpuReadbackTriggerContext<'_, D::Privilege>,
    ) -> Result<u64, AdapterError> {
        let image = ctx.image.ok_or(AdapterError::BackendRejected {
            reason:
                "InProcessCpuReadbackCopyTrigger requires a source VkImage; consumer-flavor surfaces have none"
                    .into(),
        })?;
        self.submit_image_buffer_copy(
            image,
            ctx.from_layout,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            CopyDirection::ImageToBuffer,
            ctx,
        )
    }

    fn run_copy_buffer_to_image(
        &self,
        ctx: &CpuReadbackTriggerContext<'_, D::Privilege>,
    ) -> Result<u64, AdapterError> {
        let image = ctx.image.ok_or(AdapterError::BackendRejected {
            reason: "InProcessCpuReadbackCopyTrigger requires a source VkImage on flush".into(),
        })?;
        self.submit_image_buffer_copy(
            image,
            ctx.from_layout,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            CopyDirection::BufferToImage,
            ctx,
        )
    }
}

#[cfg(target_os = "linux")]
impl<D: VulkanRhiDevice> Drop for InProcessCpuReadbackCopyTrigger<D> {
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

#[derive(Clone, Copy)]
enum CopyDirection {
    ImageToBuffer,
    BufferToImage,
}

/// Direction enum used by [`CpuReadbackSurfaceAdapter::run_bridge_copy_*`]
/// internally. Kept private — public callers use the two
/// `run_bridge_copy_image_to_buffer` / `run_bridge_copy_buffer_to_image`
/// methods so the wire-shape mapping stays explicit at the call site.
#[derive(Clone, Copy)]
enum BridgeDirection {
    ImageToBuffer,
    BufferToImage,
}

#[cfg(target_os = "linux")]
impl<D: VulkanRhiDevice + 'static> InProcessCpuReadbackCopyTrigger<D> {
    fn submit_image_buffer_copy<P: DevicePrivilege>(
        &self,
        image: vk::Image,
        from_layout: vk::ImageLayout,
        transfer_layout: vk::ImageLayout,
        direction: CopyDirection,
        ctx: &CpuReadbackTriggerContext<'_, P>,
    ) -> Result<u64, AdapterError> {
        let vk_device = self.device.device();
        let queue = self.device.queue();
        let qf = self.device.queue_family_index();
        let combined_aspect = combined_aspect_mask(ctx.format);

        let mut guard = self
            .submit_ctx
            .lock()
            .map_err(|_| AdapterError::BackendRejected {
                reason: "submit_image_buffer_copy: persistent submit context mutex poisoned"
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
        unsafe { vk_device.begin_command_buffer(cmd, &begin_info) }
            .map_err(|e| AdapterError::BackendRejected {
                reason: format!("begin_command_buffer: {e}"),
            })?;

        let pre_barrier =
            build_image_barrier(image, qf, from_layout, transfer_layout, combined_aspect);
        let pre_barriers = [pre_barrier];
        let pre_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&pre_barriers)
            .build();
        unsafe { vk_device.cmd_pipeline_barrier2(cmd, &pre_dep) };

        for (plane_idx, plane) in ctx.planes.iter().enumerate() {
            let aspect = plane_aspect_mask(ctx.format, plane_idx as u32);
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
            match direction {
                CopyDirection::ImageToBuffer => unsafe {
                    vk_device.cmd_copy_image_to_buffer(
                        cmd,
                        image,
                        transfer_layout,
                        plane.buffer,
                        &[copy_region],
                    )
                },
                CopyDirection::BufferToImage => unsafe {
                    vk_device.cmd_copy_buffer_to_image(
                        cmd,
                        plane.buffer,
                        image,
                        transfer_layout,
                        &[copy_region],
                    )
                },
            }
        }

        let post_image_barrier = build_image_barrier(
            image,
            qf,
            transfer_layout,
            vk::ImageLayout::GENERAL,
            combined_aspect,
        );
        let post_image_barriers = [post_image_barrier];
        let post_buf_barriers: Vec<vk::BufferMemoryBarrier2> = match direction {
            CopyDirection::ImageToBuffer => ctx
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
                .collect(),
            CopyDirection::BufferToImage => Vec::new(),
        };
        let post_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&post_image_barriers)
            .buffer_memory_barriers(&post_buf_barriers)
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
            .semaphore(ctx.timeline.semaphore())
            .value(ctx.suggested_signal_value)
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

        Ok(ctx.suggested_signal_value)
    }
}

/// Persistent per-trigger / per-adapter command pool, command buffer,
/// and completion fence — replaces the create-and-destroy-per-submit
/// pattern that used to churn `vkCreateCommandPool` /
/// `vkDestroyCommandPool` once per copy. Same shape lives in
/// `streamlib-adapter-cuda::adapter::AdapterPersistentSubmitContext`;
/// fix BOTH if you change EITHER (issue #620 AI Agent Notes).
///
/// The fence is created signaled so the first submit doesn't block
/// waiting on a previous-submit completion. Subsequent submits wait
/// on the fence (instant if the prior submit has already drained,
/// which is the steady state for cpu-readback because the adapter
/// already CPU-waits on the timeline before the customer reads).
/// `vkResetCommandPool` is the cheap path per Vulkan spec — recycles
/// every command buffer's memory in one call.
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
        unsafe {
            device.reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty())
        }
        .map_err(|e| AdapterError::BackendRejected {
            reason: format!("reset_command_pool (persistent submit pool): {e}"),
        })?;
        Ok(())
    }

    /// Tear down the pool + fence. Caller must guarantee the fence is
    /// signaled (no GPU work pending) — `Drop` paths satisfy this by
    /// either waiting on the fence first or only destroying after a
    /// known-completed submit.
    fn destroy(self, device: &vulkanalia::Device) {
        // Wait for any pending submit to drain so destruction is safe.
        let _ =
            unsafe { device.wait_for_fences(&[self.fence], true, u64::MAX) };
        unsafe {
            device.destroy_fence(self.fence, None);
            device.destroy_command_pool(self.pool, None);
        }
    }
}

// =====================================================================
// Internal data structures and helpers.
// =====================================================================

#[derive(Clone, Copy)]
struct PlaneAcquireSlot {
    buffer: vk::Buffer,
    mapped_ptr: *mut u8,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
    byte_size: u64,
}

struct AcquireSnapshot<P: DevicePrivilege> {
    timeline: Arc<P::TimelineSemaphore>,
    wait_value: u64,
    image: Option<vk::Image>,
    from: VulkanLayout,
    format: SurfaceFormat,
    width: u32,
    height: u32,
    planes: Vec<PlaneAcquireSlot>,
    _marker: PhantomData<P>,
}

unsafe impl<P: DevicePrivilege> Send for AcquireSnapshot<P> {}
unsafe impl<P: DevicePrivilege> Sync for AcquireSnapshot<P> {}

struct AcquireOutcome<P: DevicePrivilege> {
    snap: AcquireSnapshot<P>,
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

fn plane_aspect_mask(format: SurfaceFormat, plane: u32) -> vk::ImageAspectFlags {
    match (format, plane) {
        (SurfaceFormat::Bgra8 | SurfaceFormat::Rgba8, 0) => vk::ImageAspectFlags::COLOR,
        (SurfaceFormat::Nv12, 0) => vk::ImageAspectFlags::PLANE_0,
        (SurfaceFormat::Nv12, 1) => vk::ImageAspectFlags::PLANE_1,
        _ => unreachable!("plane_aspect_mask: plane {plane} out of range for {format:?}"),
    }
}

fn combined_aspect_mask(format: SurfaceFormat) -> vk::ImageAspectFlags {
    match format {
        SurfaceFormat::Bgra8 | SurfaceFormat::Rgba8 => vk::ImageAspectFlags::COLOR,
        SurfaceFormat::Nv12 => vk::ImageAspectFlags::PLANE_0 | vk::ImageAspectFlags::PLANE_1,
    }
}

fn build_read_view<'g, P: DevicePrivilege>(snap: &AcquireSnapshot<P>) -> CpuReadbackReadView<'g> {
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

fn build_write_view<'g, P: DevicePrivilege>(snap: &AcquireSnapshot<P>) -> CpuReadbackWriteView<'g> {
    let planes = snap
        .planes
        .iter()
        .map(|p| CpuReadbackPlaneViewMut {
            bytes: unsafe { std::slice::from_raw_parts_mut(p.mapped_ptr, p.byte_size as usize) },
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
