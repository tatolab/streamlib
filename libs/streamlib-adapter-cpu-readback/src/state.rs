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

use std::sync::Arc;

use streamlib_consumer_rhi::{DevicePrivilege, VulkanLayout};
use streamlib_adapter_abi::{SurfaceFormat, SurfaceId, SurfaceRegistration};

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
    /// Per-process monotonic signal counter for whichever side this
    /// adapter instance signals. The producer trigger advances this on
    /// every `vkCmdCopy*` submit (producer-side); the
    /// consumer-flavored `end_read_access` advances this on each
    /// `signal_host(consume_done)` call.
    pub(crate) current_signal_value: u64,
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
