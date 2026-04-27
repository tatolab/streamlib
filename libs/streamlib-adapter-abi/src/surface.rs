// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `StreamlibSurface` descriptor and supporting types.
//!
//! The descriptor is `#[repr(C)]` and crosses every language boundary —
//! Rust, Python (ctypes), Deno (UnsafePointerView). Customer-visible
//! fields are `pub`; transport and sync fields are `pub(crate)` and only
//! reachable from adapter implementations through the accessors below.

use bitflags::bitflags;

/// Host-assigned identifier for a surface.
pub type SurfaceId = u64;

/// Pixel format the surface is allocated in.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SurfaceFormat {
    Bgra8 = 0,
    Rgba8 = 1,
    Nv12 = 2,
}

bitflags! {
    /// What a surface may be used for.
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
    pub struct SurfaceUsage: u32 {
        const RENDER_TARGET = 1 << 0;
        const SAMPLED       = 1 << 1;
        const CPU_READBACK  = 1 << 2;
    }
}

/// Wire-format access mode used by the IPC and polyglot mirrors.
///
/// The Rust trait uses typestate (separate `acquire_read`/`acquire_write`
/// methods) — this enum exists for the polyglot SDKs and the on-the-wire
/// representation, where typestate doesn't translate ergonomically.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AccessMode {
    Read = 0,
    Write = 1,
}

/// Maximum number of DMA-BUF planes carried in a single surface descriptor.
///
/// Matches `streamlib_surface_client::MAX_DMA_BUF_PLANES`. Kept in sync
/// here so the descriptor stays a single-translation-unit `#[repr(C)]`
/// type without dragging that crate as a dep.
pub const MAX_DMA_BUF_PLANES: usize = 4;

/// Adapter-internal: how the surface's backing pixels are transported.
///
/// On Linux this carries DMA-BUF fds + per-plane offsets/strides + the
/// DRM format modifier of the underlying `VkImage`. The fields are
/// `pub(crate)` — only reachable inside this crate, and only from
/// adapter implementations that use them through the accessors on
/// `StreamlibSurface`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SurfaceTransportHandle {
    /// Number of valid plane fds in [`Self::dma_buf_fds`].
    pub(crate) plane_count: u32,
    /// DMA-BUF fds, one per plane. Slots beyond `plane_count` are -1.
    pub(crate) dma_buf_fds: [i32; MAX_DMA_BUF_PLANES],
    /// Per-plane offset into the fd's mmap region.
    pub(crate) plane_offsets: [u64; MAX_DMA_BUF_PLANES],
    /// Per-plane row stride in bytes.
    pub(crate) plane_strides: [u64; MAX_DMA_BUF_PLANES],
    /// DRM format modifier of the underlying `VkImage` (linear or tiled).
    pub(crate) drm_format_modifier: u64,
}

impl SurfaceTransportHandle {
    /// Empty transport handle — slots all `-1` / `0`.
    ///
    /// Used by adapters that don't go through DMA-BUF (e.g. CPU readback)
    /// and by tests.
    pub const fn empty() -> Self {
        Self {
            plane_count: 0,
            dma_buf_fds: [-1; MAX_DMA_BUF_PLANES],
            plane_offsets: [0; MAX_DMA_BUF_PLANES],
            plane_strides: [0; MAX_DMA_BUF_PLANES],
            drm_format_modifier: 0,
        }
    }

    /// Construct from the per-plane arrays. Called by the host side when
    /// registering a backing with an adapter; the descriptor is then
    /// passed to consumer adapters across IPC.
    pub const fn new(
        plane_count: u32,
        dma_buf_fds: [i32; MAX_DMA_BUF_PLANES],
        plane_offsets: [u64; MAX_DMA_BUF_PLANES],
        plane_strides: [u64; MAX_DMA_BUF_PLANES],
        drm_format_modifier: u64,
    ) -> Self {
        Self {
            plane_count,
            dma_buf_fds,
            plane_offsets,
            plane_strides,
            drm_format_modifier,
        }
    }

    pub const fn plane_count(&self) -> u32 {
        self.plane_count
    }

    pub const fn dma_buf_fds(&self) -> &[i32; MAX_DMA_BUF_PLANES] {
        &self.dma_buf_fds
    }

    pub const fn plane_offsets(&self) -> &[u64; MAX_DMA_BUF_PLANES] {
        &self.plane_offsets
    }

    pub const fn plane_strides(&self) -> &[u64; MAX_DMA_BUF_PLANES] {
        &self.plane_strides
    }

    pub const fn drm_format_modifier(&self) -> u64 {
        self.drm_format_modifier
    }
}

/// Adapter-internal: timeline-semaphore handles, counters, and the
/// current image layout.
///
/// The release-side semaphore value advances inside `acquire_*` /
/// `end_*_access` (sealed adapter methods called by guard `Drop`).
/// Customers never touch these fields.
///
/// Subprocess adapters cannot dereference `timeline_semaphore_handle`
/// directly — it's a host-side `VkSemaphore`. They import
/// `timeline_semaphore_sync_fd` via `vkImportSemaphoreFdKHR` to wait
/// or signal on the same timeline. Host-Rust adapters typically use
/// the handle and ignore the fd.
///
/// `_reserved` is 16 bytes of zeroed space for additive ABI extensions
/// before the next major bump (additional fds, opaque per-vendor sync
/// state, etc.).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SurfaceSyncState {
    /// Opaque host-side `VkSemaphore` handle.
    pub(crate) timeline_semaphore_handle: u64,
    /// Sync-fd exported via `vkGetSemaphoreFdKHR`; -1 when unset.
    pub(crate) timeline_semaphore_sync_fd: i32,
    pub(crate) _pad_a: u32,
    /// Last acquire-side wait value the host signaled.
    pub(crate) last_acquire_value: u64,
    /// Last release-side signal value the host saw.
    pub(crate) last_release_value: u64,
    /// Current `VkImageLayout` (i32 per Vulkan spec).
    pub(crate) current_image_layout: i32,
    pub(crate) _pad_b: u32,
    /// Reserved bytes for additive ABI extensions. MUST be zeroed.
    pub(crate) _reserved: [u8; 16],
}

impl SurfaceSyncState {
    /// Construct from the host-side semaphore + sync-fd + initial layout.
    ///
    /// Subprocess consumer adapters never call this — they receive the
    /// state in a [`StreamlibSurface`] over IPC and read fields via the
    /// accessors below. Only the host-side surface-share registration
    /// path constructs from raw values.
    pub const fn new(
        timeline_semaphore_handle: u64,
        timeline_semaphore_sync_fd: i32,
        last_acquire_value: u64,
        last_release_value: u64,
        current_image_layout: i32,
    ) -> Self {
        Self {
            timeline_semaphore_handle,
            timeline_semaphore_sync_fd,
            _pad_a: 0,
            last_acquire_value,
            last_release_value,
            current_image_layout,
            _pad_b: 0,
            _reserved: [0; 16],
        }
    }

    pub const fn timeline_semaphore_handle(&self) -> u64 {
        self.timeline_semaphore_handle
    }

    pub const fn timeline_semaphore_sync_fd(&self) -> i32 {
        self.timeline_semaphore_sync_fd
    }

    pub const fn last_acquire_value(&self) -> u64 {
        self.last_acquire_value
    }

    pub const fn last_release_value(&self) -> u64 {
        self.last_release_value
    }

    pub const fn current_image_layout(&self) -> i32 {
        self.current_image_layout
    }
}

/// Stable, customer-visible descriptor for a shared GPU surface.
///
/// `id`, `width`, `height`, `format`, and `usage` are public. The
/// `transport` and `sync` fields are `pub(crate)` and only reachable
/// from this crate — adapter implementations consume them via the
/// `pub(crate)` accessors below; customers never touch them.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct StreamlibSurface {
    pub id: SurfaceId,
    pub width: u32,
    pub height: u32,
    pub format: SurfaceFormat,
    pub usage: SurfaceUsage,
    pub(crate) transport: SurfaceTransportHandle,
    pub(crate) sync: SurfaceSyncState,
}

impl StreamlibSurface {
    /// Construct a surface descriptor from its public fields plus
    /// adapter-internal transport/sync state.
    ///
    /// Adapter implementations call this; customers receive surfaces
    /// from the runtime and never construct them directly.
    pub fn new(
        id: SurfaceId,
        width: u32,
        height: u32,
        format: SurfaceFormat,
        usage: SurfaceUsage,
        transport: SurfaceTransportHandle,
        sync: SurfaceSyncState,
    ) -> Self {
        Self {
            id,
            width,
            height,
            format,
            usage,
            transport,
            sync,
        }
    }

    /// Adapter-facing: the DMA-BUF transport handle (fds, plane layout,
    /// modifier).
    ///
    /// Customer code should not need this — it sees a framework-shaped
    /// view through the adapter's `acquire_*` methods. Adapters reach
    /// for it to import the backing on the consumer side
    /// (`VkImportMemoryFdInfoKHR`, `eglCreateImageKHR`, …).
    pub const fn transport(&self) -> &SurfaceTransportHandle {
        &self.transport
    }

    /// Adapter-facing: the host-side timeline-semaphore + initial layout.
    ///
    /// Subprocess adapters import `timeline_semaphore_sync_fd` via
    /// `vkImportSemaphoreFdKHR` and wait/signal the same timeline values
    /// the host advances; the opaque `timeline_semaphore_handle` is
    /// host-only and not dereferenceable in another address space.
    pub const fn sync(&self) -> &SurfaceSyncState {
        &self.sync
    }
}

#[cfg(test)]
mod tests {
    //! Layout regression suite for the descriptor types.
    //!
    //! These numbers are copied verbatim into the Python ctypes mirror
    //! (`libs/streamlib-python/python/streamlib/surface_adapter.py`) and
    //! the Deno UnsafePointerView reader (`libs/streamlib-deno/
    //! surface_adapter.ts`). When this file changes, both polyglot
    //! mirrors must be updated in the same commit; their twin tests
    //! lock the same offsets from the other side.

    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn surface_format_is_u32() {
        assert_eq!(size_of::<SurfaceFormat>(), 4);
        assert_eq!(align_of::<SurfaceFormat>(), 4);
    }

    #[test]
    fn surface_usage_is_repr_transparent_u32() {
        assert_eq!(size_of::<SurfaceUsage>(), 4);
        assert_eq!(align_of::<SurfaceUsage>(), 4);
    }

    #[test]
    fn access_mode_is_u32() {
        assert_eq!(size_of::<AccessMode>(), 4);
    }

    #[test]
    fn transport_handle_empty_marks_fds_invalid() {
        let t = SurfaceTransportHandle::empty();
        assert_eq!(t.plane_count, 0);
        for fd in t.dma_buf_fds {
            assert_eq!(fd, -1);
        }
    }

    #[test]
    fn surface_transport_handle_layout() {
        // plane_count: u32 @ 0
        // dma_buf_fds: [i32; 4] @ 4 (16 B)
        // plane_offsets: [u64; 4] needs u64 alignment — pad 20 → 24
        // plane_strides: [u64; 4] @ 56
        // drm_format_modifier: u64 @ 88
        assert_eq!(offset_of!(SurfaceTransportHandle, plane_count), 0);
        assert_eq!(offset_of!(SurfaceTransportHandle, dma_buf_fds), 4);
        assert_eq!(offset_of!(SurfaceTransportHandle, plane_offsets), 24);
        assert_eq!(offset_of!(SurfaceTransportHandle, plane_strides), 56);
        assert_eq!(
            offset_of!(SurfaceTransportHandle, drm_format_modifier),
            88
        );
        assert_eq!(size_of::<SurfaceTransportHandle>(), 96);
        assert_eq!(align_of::<SurfaceTransportHandle>(), 8);
    }

    #[test]
    fn surface_sync_state_layout() {
        // timeline_semaphore_handle: u64 @ 0
        // timeline_semaphore_sync_fd: i32 @ 8
        // _pad_a: u32 @ 12
        // last_acquire_value: u64 @ 16
        // last_release_value: u64 @ 24
        // current_image_layout: i32 @ 32
        // _pad_b: u32 @ 36
        // _reserved: [u8; 16] @ 40
        // total: 56 bytes, align 8
        assert_eq!(offset_of!(SurfaceSyncState, timeline_semaphore_handle), 0);
        assert_eq!(
            offset_of!(SurfaceSyncState, timeline_semaphore_sync_fd),
            8
        );
        assert_eq!(offset_of!(SurfaceSyncState, _pad_a), 12);
        assert_eq!(offset_of!(SurfaceSyncState, last_acquire_value), 16);
        assert_eq!(offset_of!(SurfaceSyncState, last_release_value), 24);
        assert_eq!(offset_of!(SurfaceSyncState, current_image_layout), 32);
        assert_eq!(offset_of!(SurfaceSyncState, _pad_b), 36);
        assert_eq!(offset_of!(SurfaceSyncState, _reserved), 40);
        assert_eq!(size_of::<SurfaceSyncState>(), 56);
        assert_eq!(align_of::<SurfaceSyncState>(), 8);
    }

    /// Pub accessors return the values stored at construction. Locks the
    /// "consumer adapter can read transport / sync state" contract — if
    /// the accessors stop returning the field they refer to, every
    /// subprocess adapter breaks at runtime.
    #[test]
    fn pub_accessors_round_trip_constructed_values() {
        let transport = SurfaceTransportHandle::new(
            2,
            [10, 11, -1, -1],
            [0, 4096, 0, 0],
            [1920, 1920, 0, 0],
            0x123_4567_89ab_cdef,
        );
        assert_eq!(transport.plane_count(), 2);
        assert_eq!(transport.dma_buf_fds(), &[10, 11, -1, -1]);
        assert_eq!(transport.plane_offsets(), &[0, 4096, 0, 0]);
        assert_eq!(transport.plane_strides(), &[1920, 1920, 0, 0]);
        assert_eq!(transport.drm_format_modifier(), 0x123_4567_89ab_cdef);

        let sync = SurfaceSyncState::new(0xdead_beef, 42, 7, 5, 1);
        assert_eq!(sync.timeline_semaphore_handle(), 0xdead_beef);
        assert_eq!(sync.timeline_semaphore_sync_fd(), 42);
        assert_eq!(sync.last_acquire_value(), 7);
        assert_eq!(sync.last_release_value(), 5);
        assert_eq!(sync.current_image_layout(), 1);

        let surface = StreamlibSurface::new(
            99,
            1920,
            1080,
            SurfaceFormat::Bgra8,
            SurfaceUsage::RENDER_TARGET,
            transport,
            sync,
        );
        assert_eq!(surface.id, 99);
        assert_eq!(surface.transport().plane_count(), 2);
        assert_eq!(surface.sync().timeline_semaphore_sync_fd(), 42);
    }

    #[test]
    fn streamlib_surface_layout() {
        // id: u64 @ 0
        // width: u32 @ 8
        // height: u32 @ 12
        // format: u32 @ 16
        // usage: u32 @ 20
        // transport: SurfaceTransportHandle (96 B, align 8) @ 24
        // sync: SurfaceSyncState (56 B, align 8) @ 120
        // total: 176 bytes, align 8
        assert_eq!(offset_of!(StreamlibSurface, id), 0);
        assert_eq!(offset_of!(StreamlibSurface, width), 8);
        assert_eq!(offset_of!(StreamlibSurface, height), 12);
        assert_eq!(offset_of!(StreamlibSurface, format), 16);
        assert_eq!(offset_of!(StreamlibSurface, usage), 20);
        assert_eq!(offset_of!(StreamlibSurface, transport), 24);
        assert_eq!(offset_of!(StreamlibSurface, sync), 120);
        assert_eq!(size_of::<StreamlibSurface>(), 176);
        assert_eq!(align_of::<StreamlibSurface>(), 8);
    }
}
