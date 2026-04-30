// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-surface adapter state.
//!
//! `CudaSurfaceAdapter<D>` is generic over the device flavor (`HostMarker`
//! today; future cdylib work will add a consumer flavor). The structs in
//! this module carry the privilege parameter through so the pixel-buffer
//! and timeline-semaphore types resolve to the matching flavor (`Host*`
//! or, eventually, `Consumer*`) at instantiation.
//!
//! The CUDA adapter mirrors the *crate split* of
//! `streamlib-adapter-cpu-readback` (adapter crate + helpers crate) and
//! the *struct shape* of `streamlib-adapter-vulkan` (no per-acquire
//! trigger or bridge — the host registers the OPAQUE_FD-exportable
//! resource and the carve-out test imports it into CUDA directly,
//! because GPU-resident inference dispatches inside the CUDA context
//! without needing per-acquire host work).

use std::sync::Arc;

use streamlib_adapter_abi::{SurfaceId, SurfaceRegistration};
use streamlib_consumer_rhi::{DevicePrivilege, VulkanLayout};

/// Inputs handed to [`crate::CudaSurfaceAdapter::register_host_surface`].
///
/// On the host side `pixel_buffer` is a fresh
/// `Arc<HostVulkanPixelBuffer>` from `HostVulkanPixelBuffer::new_opaque_fd_export`
/// (the OPAQUE_FD-exportable HOST_VISIBLE staging buffer the CUDA carve-out
/// test imports via `cudaImportExternalMemory`). The adapter holds the
/// `Arc` for the surface's lifetime so the underlying GPU memory stays
/// alive while CUDA references the imported handle.
pub struct HostSurfaceRegistration<P: DevicePrivilege> {
    /// OPAQUE_FD-exportable HOST_VISIBLE staging buffer — the resource
    /// CUDA imports via `cudaImportExternalMemory`. Host- or consumer-
    /// flavored per `P`.
    pub pixel_buffer: Arc<P::PixelBuffer>,
    /// Timeline semaphore — host- or consumer-flavored per `P`. Both
    /// flavors implement
    /// [`streamlib_consumer_rhi::VulkanTimelineSemaphoreLike`] so the
    /// adapter's wait + signal calls work uniformly.
    pub timeline: Arc<P::TimelineSemaphore>,
    /// Initial layout the resource is in at registration time. For
    /// pixel buffers this is unused (buffers don't have layouts), but
    /// the field is kept for shape parity with `streamlib-adapter-vulkan`
    /// so the future #589/#590 work can add VkImage support without
    /// breaking the registration shape. Pass [`VulkanLayout::UNDEFINED`].
    pub initial_layout: VulkanLayout,
}

/// Per-surface state held inside the adapter's `Registry<...>`.
///
/// All mutation goes through the registry's locking so `acquire_*` /
/// `end_*_access` stay sequenced — the trait's `&self` shape is
/// satisfied by interior mutability.
pub(crate) struct SurfaceState<P: DevicePrivilege> {
    #[allow(dead_code)] // kept for tracing / debug output, not read in hot paths
    pub(crate) surface_id: SurfaceId,
    pub(crate) pixel_buffer: Arc<P::PixelBuffer>,
    pub(crate) timeline: Arc<P::TimelineSemaphore>,
    #[allow(dead_code)] // shape-parity with the Vulkan adapter; read by future image support
    pub(crate) current_layout: VulkanLayout,
    pub(crate) read_holders: u64,
    pub(crate) write_held: bool,
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
