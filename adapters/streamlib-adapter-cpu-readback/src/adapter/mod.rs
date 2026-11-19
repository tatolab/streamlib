// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
//! A consumer one process away does not come through this crate at
//! all: CPU readback across a process boundary is a `GpuContext`
//! capability, reached over the escalate ops, staged in engine-owned
//! host-visible memory.

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use streamlib_consumer_rhi::{
    DevicePrivilege, VulkanRhiBuffer, VulkanRhiDevice, VulkanTextureLike,
    VulkanTimelineSemaphoreLike,
};
use streamlib_surface_adapter::{AdapterError, Registry, SurfaceFormat, SurfaceId};
use tracing::instrument;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use streamlib_consumer_rhi::VulkanLayout;

use crate::state::{
    HostSurfaceRegistration, PlaneSlot, SurfaceState, SurfaceTimelineSignalSequence,
};
use crate::view::{
    CpuReadbackPlaneView, CpuReadbackPlaneViewMut, CpuReadbackReadView, CpuReadbackWriteView,
};

mod adapter_persistent_submit_context;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod adapter_persistent_submit_context_tests;
mod in_process_cpu_readback_copy_trigger;
mod surface_adapter_for_cpu_readback;

pub use in_process_cpu_readback_copy_trigger::InProcessCpuReadbackCopyTrigger;

/// Default per-acquire wait timeout. Bounds each timeline wait
/// individually, and nothing else: not the trigger call — the in-process
/// trigger blocks untimed on the prior submit's fence — and not the
/// acquire as a whole, which first blocks untimed on the surface's
/// signal sequence while another caller runs its copy.
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
    /// Producer's `produce_done` timeline — the host trigger signals
    /// it via `vkQueueSubmit2::pSignalSemaphoreInfos` on the GPU copy
    /// submit; the subprocess trigger signals it via the IPC bridge's
    /// host-side counterpart. Consumer-flavor adapters wait on this
    /// timeline post-trigger to confirm the copy completed before
    /// reading the staging buffer. Single-writer-per-edge per
    /// the single-writer-per-edge timeline rule.
    pub produce_done: &'a Arc<P::TimelineSemaphore>,
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
///
/// Two contracts the code's shape cannot show. The returned value MUST
/// be at least `ctx.suggested_signal_value`: a timeline wait is
/// satisfied by any value at or above its target, so a lower one may
/// already be signaled and would let the adapter's wait return while
/// this copy is still running. And a trigger MUST NOT re-enter the
/// adapter for the same surface — it is called under that surface's
/// signal sequence, which is not reentrant.
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

            // The staging buffer's byte size must match the plane's
            // logical byte size (`pw * ph * pbpp`). Caller's responsibility
            // to size the underlying `Vk::Buffer` allocation correctly —
            // pixel-shape lives on `PlaneSlot`, not on the bottom-layer
            // primitive, so we validate against `size()` only.
            let expected_size = (pw as u64) * (ph as u64) * (pbpp as u64);
            let actual_size = staging.size();
            if actual_size != expected_size {
                return Err(AdapterError::UnsupportedFormat {
                    surface_id: id,
                    reason: format!(
                        "plane {plane_idx} staging size mismatch: expected {expected_size} bytes \
                         ({pw}x{ph}@{pbpp}bpp), got {actual_size}"
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
            texture: registration.texture,
            planes,
            produce_done: registration.produce_done,
            consume_done: registration.consume_done,
            current_layout: registration.initial_image_layout,
            read_holders: 0,
            write_held: false,
            signal_sequence: Arc::new(SurfaceTimelineSignalSequence::default()),
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

    fn snapshot_surface_for_copy_submit(
        state: &mut SurfaceState<D::Privilege>,
    ) -> SurfaceCopySnapshot<D::Privilege> {
        let produce_done = Arc::clone(&state.produce_done);
        let consume_done = Arc::clone(&state.consume_done);
        let signal_sequence = Arc::clone(&state.signal_sequence);
        let image = state.texture.as_ref().and_then(|t| t.image());
        let from = state.current_layout;
        let format = state.format;
        let width = state.width;
        let height = state.height;
        let plane_snaps: Vec<SurfaceCopyPlaneSlot> = state
            .planes
            .iter()
            .map(|p| SurfaceCopyPlaneSlot {
                buffer: p.staging.buffer(),
                mapped_ptr: p.staging.mapped_ptr(),
                width: p.width,
                height: p.height,
                bytes_per_pixel: p.bytes_per_pixel,
                byte_size: p.byte_size(),
            })
            .collect();
        SurfaceCopySnapshot {
            produce_done,
            consume_done,
            signal_sequence,
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
    ) -> Result<Option<SurfaceCopySnapshot<D::Privilege>>, AdapterError> {
        self.surfaces.try_begin_read(surface_id, |state| {
            Ok(Self::snapshot_surface_for_copy_submit(state))
        })
    }

    fn try_begin_write_inner(
        &self,
        surface_id: SurfaceId,
    ) -> Result<Option<SurfaceCopySnapshot<D::Privilege>>, AdapterError> {
        self.surfaces.try_begin_write(surface_id, |state| {
            Ok(Self::snapshot_surface_for_copy_submit(state))
        })
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
        snap: &'a SurfaceCopySnapshot<D::Privilege>,
        suggested_signal_value: u64,
        trigger_planes: &'a [TriggerPlane],
    ) -> CpuReadbackTriggerContext<'a, D::Privilege> {
        CpuReadbackTriggerContext {
            surface_id,
            image: snap.image,
            from_layout: snap.from.as_vk(),
            format: snap.format,
            produce_done: &snap.produce_done,
            planes: trigger_planes,
            queue_family_index: self.device.queue_family_index(),
            suggested_signal_value,
        }
    }

    fn log_acquire(
        &self,
        surface_id: SurfaceId,
        snap: &SurfaceCopySnapshot<D::Privilege>,
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

    /// Reserve the surface's next `produce_done` value, run the copy
    /// that signals it, and wait for the value the trigger reports.
    ///
    /// The reservation is held across all three. Because a timeline wait
    /// is satisfied by any value at or above its target, releasing it
    /// after the submit would let a second caller's copy signal a higher
    /// value and satisfy the first caller's wait while the first copy is
    /// still running — the defect this serialization exists to remove.
    /// Holding it means at most one copy signals `produce_done` for a
    /// surface at a time, which the adapter guarantees rather than
    /// delegating to whichever trigger is installed — on the success
    /// path. Both error paths below return after the trigger has already
    /// submitted, dropping the reservation with that copy outstanding;
    /// the acquire has failed by then, and what keeps the next one
    /// ordered behind it is the trigger's own serialization. See
    /// the single-writer-per-edge timeline rule, thread
    /// model within the writer process.
    fn submit_copy_and_await_produce_done(
        &self,
        surface_id: SurfaceId,
        snap: &SurfaceCopySnapshot<D::Privilege>,
        direction: CopyDirection,
    ) -> Result<(), AdapterError> {
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
        let mut reserved = snap.signal_sequence.reserve_next_signal_value();
        let ctx = self.make_trigger_context(surface_id, snap, reserved.value(), &trigger_planes);
        let signaled = match direction {
            CopyDirection::ImageToBuffer => self.trigger.run_copy_image_to_buffer(&ctx)?,
            CopyDirection::BufferToImage => self.trigger.run_copy_buffer_to_image(&ctx)?,
        };
        reserved.observe_signaled_value(signaled).map_err(|below| {
            AdapterError::BackendRejected {
                reason: format!(
                    "trigger reported produce_done value {signaled} for surface_id={surface_id}, \
                     below the reserved {}; a value this surface may already carry cannot prove \
                     the copy completed",
                    below.reserved
                ),
            }
        })?;
        snap.produce_done
            .wait(signaled, self.acquire_timeout.as_nanos() as u64)
            .map_err(|_| AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            })?;
        Ok(())
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

        // Pre-trigger wait: the producer trigger advances
        // `produce_done`, so a read acquire confirms prior writes
        // have drained on `produce_done`; a write acquire confirms
        // prior reads have drained on `consume_done`. Both are no-ops
        // on the first acquire (a fresh timeline is at value 0 and
        // `wait(0)` returns immediately). Single-writer-per-edge per
        // the single-writer-per-edge timeline rule.
        let pre_wait_target = if write {
            &snap.consume_done
        } else {
            &snap.produce_done
        };
        let pre_wait_value = pre_wait_target.current_value().unwrap_or(0);
        if pre_wait_target
            .wait(pre_wait_value, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            self.rollback_acquire(surface_id, write);
            return Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            });
        }

        if let Err(e) =
            self.submit_copy_and_await_produce_done(surface_id, &snap, CopyDirection::ImageToBuffer)
        {
            self.rollback_acquire(surface_id, write);
            return Err(e);
        }
        self.surfaces.with_mut(surface_id, |state| {
            state.current_layout = VulkanLayout::GENERAL;
        });

        Ok(Some(AcquireOutcome { snap }))
    }
}

// =====================================================================
// Internal data structures and helpers.
// =====================================================================

#[derive(Clone, Copy)]
enum CopyDirection {
    ImageToBuffer,
    BufferToImage,
}

/// What `end_read_access` still owes the surface once it has dropped the
/// registry lock. Only the last reader out signals `consume_done`.
enum ConsumeDoneSignalOnReadRelease<P: DevicePrivilege> {
    NotTheLastReader,
    LastReaderMustSignal {
        consume_done: Arc<P::TimelineSemaphore>,
        signal_sequence: Arc<SurfaceTimelineSignalSequence>,
    },
}

#[derive(Clone, Copy)]
struct SurfaceCopyPlaneSlot {
    buffer: vk::Buffer,
    mapped_ptr: *mut u8,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
    byte_size: u64,
}

struct SurfaceCopySnapshot<P: DevicePrivilege> {
    /// Producer's `produce_done` timeline — the trigger signals it
    /// and the consumer waits on it. Single-writer-per-edge per
    /// the single-writer-per-edge timeline rule.
    produce_done: Arc<P::TimelineSemaphore>,
    /// Consumer's `consume_done` timeline — signaled by
    /// `end_read_access`. Snapshotted into the acquire path so the
    /// producer's write-side waits can reach it.
    consume_done: Arc<P::TimelineSemaphore>,
    /// This process's signal sequence for the surface — every copy
    /// submitted off this snapshot reserves its `produce_done` value
    /// from it and holds the reservation until its own wait returns.
    signal_sequence: Arc<SurfaceTimelineSignalSequence>,
    image: Option<vk::Image>,
    from: VulkanLayout,
    format: SurfaceFormat,
    width: u32,
    height: u32,
    planes: Vec<SurfaceCopyPlaneSlot>,
    _marker: PhantomData<P>,
}

unsafe impl<P: DevicePrivilege> Send for SurfaceCopySnapshot<P> {}
unsafe impl<P: DevicePrivilege> Sync for SurfaceCopySnapshot<P> {}

struct AcquireOutcome<P: DevicePrivilege> {
    snap: SurfaceCopySnapshot<P>,
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

fn build_read_view<'g, P: DevicePrivilege>(
    snap: &SurfaceCopySnapshot<P>,
) -> CpuReadbackReadView<'g> {
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

fn build_write_view<'g, P: DevicePrivilege>(
    snap: &SurfaceCopySnapshot<P>,
) -> CpuReadbackWriteView<'g> {
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
