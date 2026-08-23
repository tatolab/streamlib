// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-surface adapter state, generic over device-privilege flavor
//! (`HostMarker` for in-process Rust callers, `ConsumerMarker` for
//! subprocess cdylib callers).
//!
//! Both flavors store the same shape — a source `VkImage` (held but
//! only directly touched on the host side), one HOST_VISIBLE staging
//! `VkBuffer` per plane, a shared timeline semaphore, and the
//! acquire/release counters. The host pre-allocates and registers
//! everything via surface-share; the consumer imports the FDs at
//! registration time through `streamlib-consumer-rhi` and registers
//! the resulting `Consumer*` handles back into this same shape.

use std::sync::{Arc, Mutex, MutexGuard};

use streamlib_consumer_rhi::{DevicePrivilege, VulkanLayout};
use streamlib_surface_adapter::{SurfaceFormat, SurfaceId, SurfaceRegistration};

/// Inputs the registration site hands to
/// [`crate::CpuReadbackSurfaceAdapter::register_host_surface`].
///
/// Generic over [`DevicePrivilege`] so the same registration shape works
/// host-side (`P = HostMarker`) and consumer-side (`P = ConsumerMarker`).
/// Both flavors carry the same fields but the concrete types behind the
/// `Arc`s differ:
///
/// - Host: `Arc<HostVulkanTexture>` + `Vec<Arc<HostVulkanBuffer>>` +
///   `Arc<HostVulkanTimelineSemaphore>` — pre-allocated through the host
///   RHI and registered with the surface-share service so subprocesses can
///   import them.
/// - Consumer: `Arc<ConsumerVulkanTexture>` (placeholder — the consumer
///   typically does not import the source image; image transitions are
///   host-only) + `Vec<Arc<ConsumerVulkanBuffer>>` (imported via
///   `from_dma_buf_fds`) + `Arc<ConsumerVulkanTimelineSemaphore>`
///   (imported via `from_imported_opaque_fd`).
pub struct HostSurfaceRegistration<P: DevicePrivilege> {
    /// Source surface texture. Host-side this is the `VkImage` the
    /// adapter copies to/from; consumer-side this slot is populated only
    /// when the consumer actually imports the image (rarely needed —
    /// cpu-readback consumers operate against the staging buffer's
    /// mapped pointer, not the image).
    pub texture: Option<Arc<P::Texture>>,
    /// One staging buffer per plane (1 for BGRA/RGBA, 2 for NV12).
    /// HOST_VISIBLE / HOST_COHERENT linear `VkBuffer` on both flavors.
    pub staging_planes: Vec<Arc<P::Buffer>>,
    /// `produce_done` timeline — signaled exclusively by the producer
    /// process via the trigger's `vkQueueSubmit2::pSignalSemaphoreInfos`
    /// after `vkCmdCopyImageToBuffer` / `vkCmdCopyBufferToImage`
    /// completes. Subprocess consumers import via OPAQUE_FD and wait
    /// on it before reading the staging buffer. Single-writer-per-edge
    /// per `docs/architecture/adapter-timeline-single-writer.md`.
    pub produce_done: Arc<P::TimelineSemaphore>,
    /// `consume_done` timeline — signaled exclusively by the consumer
    /// process from `end_read_access` (CPU `signal_host`) after the
    /// subprocess has finished reading the staging buffer. The host
    /// producer waits on it before reusing the staging buffer for the
    /// next frame.
    pub consume_done: Arc<P::TimelineSemaphore>,
    /// Initial `VkImageLayout` the host left the source image in.
    /// Consumer-side this is informational — layout transitions are
    /// host-only. Use [`VulkanLayout::UNDEFINED`] for freshly-allocated
    /// images and [`VulkanLayout::GENERAL`] when the host has already
    /// transitioned the image into a copy-source-capable state.
    pub initial_image_layout: VulkanLayout,
    /// Pixel format. Drives plane count and per-plane geometry consumed
    /// by the copy paths and the customer-facing view.
    pub format: SurfaceFormat,
    /// Surface width in pixels. The adapter uses this to dimension the
    /// per-plane views; staging buffers carry their own per-plane
    /// dimensions through [`streamlib_consumer_rhi::VulkanRhiBuffer`].
    pub width: u32,
    /// Surface height in pixels.
    pub height: u32,
}

/// Per-plane staging slot. Holds an `Arc<P::Buffer>` that
/// outlives every acquire scope; the staging buffers are reused on
/// every acquire, never reallocated.
pub(crate) struct PlaneSlot<P: DevicePrivilege> {
    pub(crate) staging: Arc<P::Buffer>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bytes_per_pixel: u32,
}

impl<P: DevicePrivilege> PlaneSlot<P> {
    pub(crate) fn byte_size(&self) -> u64 {
        (self.width as u64) * (self.height as u64) * (self.bytes_per_pixel as u64)
    }
}

/// This process's monotonic signal counter for one surface, plus the
/// exclusion that makes reserving from it sound.
///
/// One counter serves both timelines: each of `produce_done` and
/// `consume_done` sees a strictly increasing subsequence, which is all
/// `VUID-VkSubmitInfo2-semaphore-03882` asks for. Gaps are legal.
///
/// The exclusion exists because reserving a distinct value is not on its
/// own enough. Two callers holding distinct values still submit in
/// whichever order they reach the queue, and a submit signalling *below*
/// the timeline's current value is as invalid as one signalling *at* it.
/// So [`reserve_next_signal_value`](Self::reserve_next_signal_value)
/// hands back a guard, and the caller holds it until its submit has been
/// issued: reservation order is then submission order, and the copies
/// retire in that order on the one queue. The wait that observes the
/// signal runs outside the guard — the value is the caller's own, so
/// nothing else can satisfy it — mirroring the engine's
/// `SurfaceExportStaging::submit_staging_copy_and_wait`.
#[derive(Default)]
pub(crate) struct SurfaceTimelineSignalSequence {
    last_reserved_signal_value: Mutex<u64>,
}

/// A reserved timeline value and the exclusion it was reserved under.
/// The sequence stays locked until this drops, so the submit that
/// signals [`value`](Self::value) cannot interleave with another
/// caller's.
#[must_use = "the reservation only holds while this guard lives — dropping it releases the sequence"]
pub(crate) struct ReservedSurfaceTimelineSignalValue<'a> {
    last_reserved_signal_value: MutexGuard<'a, u64>,
}

impl SurfaceTimelineSignalSequence {
    /// Take the next value and hold the sequence until the returned
    /// guard drops.
    pub(crate) fn reserve_next_signal_value(&self) -> ReservedSurfaceTimelineSignalValue<'_> {
        // Poison recovery: the sequence is one monotonic counter, so a
        // panic mid-submit leaves it consistent — the reserved value is
        // simply never signaled, and a gap is legal.
        let mut last_reserved_signal_value = self
            .last_reserved_signal_value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *last_reserved_signal_value += 1;
        ReservedSurfaceTimelineSignalValue {
            last_reserved_signal_value,
        }
    }
}

impl ReservedSurfaceTimelineSignalValue<'_> {
    pub(crate) fn value(&self) -> u64 {
        *self.last_reserved_signal_value
    }

    /// Fold in the value the signalling site actually used. A trigger
    /// whose host side picks its own may report one above the
    /// reservation; the next reservation must clear it.
    pub(crate) fn observe_signaled_value(&mut self, signaled: u64) {
        *self.last_reserved_signal_value = (*self.last_reserved_signal_value).max(signaled);
    }
}

/// Per-surface state held inside the adapter's
/// `Mutex<HashMap<SurfaceId, _>>`. Generic over privilege so both
/// host- and consumer-flavor adapters share the registry shape.
///
/// Layout tracking (`current_layout`) is host-side bookkeeping; the
/// consumer-side adapter never mutates it because layout transitions
/// are issued on the host's `VkDevice`.
pub(crate) struct SurfaceState<P: DevicePrivilege> {
    #[allow(dead_code)]
    pub(crate) surface_id: SurfaceId,
    pub(crate) texture: Option<Arc<P::Texture>>,
    pub(crate) planes: Vec<PlaneSlot<P>>,
    /// `produce_done` timeline — see
    /// [`HostSurfaceRegistration::produce_done`].
    pub(crate) produce_done: Arc<P::TimelineSemaphore>,
    /// `consume_done` timeline — see
    /// [`HostSurfaceRegistration::consume_done`].
    pub(crate) consume_done: Arc<P::TimelineSemaphore>,
    pub(crate) current_layout: VulkanLayout,
    pub(crate) read_holders: u64,
    pub(crate) write_held: bool,
    /// This process's signal sequence for the surface's two timelines —
    /// see [`SurfaceTimelineSignalSequence`]. Held behind an `Arc` so a
    /// signalling caller can carry it out of the registry lock and keep
    /// it for the whole reserve-submit-wait sequence.
    pub(crate) signal_sequence: Arc<SurfaceTimelineSignalSequence>,
    pub(crate) format: SurfaceFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl<P: DevicePrivilege> SurfaceRegistration for SurfaceState<P> {
    fn write_held(&self) -> bool {
        self.write_held
    }
    fn read_holders(&self) -> u64 {
        self.read_holders
    }
    fn set_write_held(&mut self, held: bool) {
        self.write_held = held;
    }
    fn inc_read_holders(&mut self) {
        self.read_holders += 1;
    }
    fn dec_read_holders(&mut self) {
        self.read_holders = self.read_holders.saturating_sub(1);
    }
}
