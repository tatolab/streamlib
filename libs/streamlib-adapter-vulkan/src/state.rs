// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-surface adapter state.
//!
//! `VulkanSurfaceAdapter<D>` is generic over the device flavor — it
//! works against either `HostVulkanDevice` (host-side allocate +
//! register) or `ConsumerVulkanDevice` (consumer-side import +
//! register). The structs in this module carry the privilege parameter
//! through so the texture and timeline-semaphore types resolve to the
//! matching flavor (`Host*` or `Consumer*`) at instantiation.

use std::sync::Arc;

use streamlib_consumer_rhi::{DevicePrivilege, VulkanLayout};
use streamlib_adapter_abi::{SurfaceId, SurfaceRegistration};

/// Inputs handed to [`crate::VulkanSurfaceAdapter::register_host_surface`].
///
/// On the host side `texture` is a fresh
/// `Arc<HostVulkanTexture>` from `HostVulkanTexture::new_render_target_dma_buf`.
/// On the consumer side it's an `Arc<ConsumerVulkanTexture>` from
/// `ConsumerVulkanTexture::import_render_target_dma_buf`. The adapter
/// reads the `vk::Image` via the `VulkanTextureLike` trait — both
/// flavors implement it — and holds the Arc as long as the surface is
/// registered, so the underlying GPU memory stays alive.
pub struct HostSurfaceRegistration<P: DevicePrivilege> {
    /// Texture wrapper — host- or consumer-flavored per `P`.
    pub texture: Arc<P::Texture>,
    /// `produce_done` timeline — signaled exclusively by the producer
    /// process when a write completes (CPU `signal_host` from
    /// `end_write_access`). The consumer waits on this timeline before
    /// reading. Single-writer-per-edge per
    /// `docs/architecture/adapter-timeline-single-writer.md`.
    pub produce_done: Arc<P::TimelineSemaphore>,
    /// `consume_done` timeline — signaled exclusively by the consumer
    /// process when a read completes (CPU `signal_host` from
    /// `end_read_access`). The producer waits on this timeline before
    /// re-writing.
    pub consume_done: Arc<P::TimelineSemaphore>,
    /// Initial layout the texture is in at registration time. The first
    /// `acquire_*` will transition from here. For freshly-allocated
    /// images this is typically [`VulkanLayout::UNDEFINED`].
    pub initial_layout: VulkanLayout,
}

/// Per-surface state held inside the adapter's `Registry<...>`.
///
/// All mutation goes through the registry's locking so `acquire_*` /
/// `end_*_access` stay sequenced — the trait's `&self` shape is
/// satisfied by interior mutability. Counters are sized to whatever
/// the underlying Vulkan timeline semaphore supports (u64);
/// `WriteContended` is a fast pre-check before the timeline wait.
pub(crate) struct SurfaceState<P: DevicePrivilege> {
    #[allow(dead_code)] // kept for tracing / debug output, not read in hot paths
    pub(crate) surface_id: SurfaceId,
    pub(crate) texture: Arc<P::Texture>,
    /// `produce_done` timeline — see
    /// [`HostSurfaceRegistration::produce_done`].
    pub(crate) produce_done: Arc<P::TimelineSemaphore>,
    /// `consume_done` timeline — see
    /// [`HostSurfaceRegistration::consume_done`].
    pub(crate) consume_done: Arc<P::TimelineSemaphore>,
    pub(crate) current_layout: VulkanLayout,
    pub(crate) read_holders: u64,
    pub(crate) write_held: bool,
    /// Last peer-timeline value the adapter waited on before handing
    /// access out. Telemetry only; the canonical wait value is the
    /// peer-timeline's kernel `current_value()` taken at finalize time.
    pub(crate) last_acquire_value: u64,
    /// Per-process monotonic signal counter. This adapter instance
    /// only writes ONE side's timeline per acquire/release cycle
    /// (`end_read_access` signals `consume_done`,
    /// `end_write_access` signals `produce_done`). The counter
    /// advances on every signal regardless of side — each timeline
    /// sees strictly monotonic values from its respective writer code
    /// path, which is all VUID-03258 requires.
    pub(crate) current_signal_value: u64,
}

impl<P: DevicePrivilege> SurfaceState<P> {
    pub(crate) fn next_signal_value(&self) -> u64 {
        self.current_signal_value + 1
    }
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
