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
    /// Timeline semaphore — host- or consumer-flavored per `P`. Both
    /// flavors implement
    /// [`streamlib_consumer_rhi::VulkanTimelineSemaphoreLike`] so
    /// the adapter's wait + signal calls work uniformly.
    pub timeline: Arc<P::TimelineSemaphore>,
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
    pub(crate) timeline: Arc<P::TimelineSemaphore>,
    pub(crate) current_layout: VulkanLayout,
    pub(crate) read_holders: u64,
    pub(crate) write_held: bool,
    /// Last value the host *waited on* before handing access out. Used
    /// only for telemetry; the canonical wait value is recomputed every
    /// acquire from `current_release_value`.
    pub(crate) last_acquire_value: u64,
    /// The value `signal_host` was last advanced to. The next acquire
    /// waits on this value (so any prior writer's GPU work has drained)
    /// and the next release advances it by one.
    pub(crate) current_release_value: u64,
}

impl<P: DevicePrivilege> SurfaceState<P> {
    pub(crate) fn next_release_value(&self) -> u64 {
        self.current_release_value + 1
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
