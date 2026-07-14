// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Plugin ABI callback table for `streamlib-adapter-vulkan`.
//!
//! Trunk piece of the per-adapter vtable lift kicked off by
//! [`streamlib-plugin-abi`]'s `GpuContextLimitedAccessVTable`
//! (issue #886). The companion 5 adapter ABIs follow this shape
//! mechanically (issues #888, #889, #890, #891, #894).
//!
//! # What this crate is
//!
//! A pure ABI contract describing how a host (`streamlib-engine`)
//! exposes its `VulkanSurfaceAdapter` to a cdylib plugin without
//! sharing any Rust types beyond `#[repr(C)]` payloads and
//! `unsafe extern "C" fn` pointers. The cdylib carries a
//! `(handle, vtable)` PluginAbiObject; the host dispatches every method
//! through host-compiled code so layout drift between rustc-minor
//! versions and divergent dep graphs is contained inside the host
//! plugin.
//!
//! Dep posture mirrors [`streamlib-plugin-abi`]: zero streamlib
//! crates pulled, zero vulkanalia, zero rustc-version-coupled
//! types. This keeps layout regression tests trivially runnable on
//! any host and the crate safe across the plugin ABI.
//!
//! # Audited cdylib-callable surface
//!
//! Every method on the cdylib-facing side of
//! `VulkanSurfaceAdapter<D>` / `VulkanContext<D>` / its acquire
//! guards is covered by exactly one slot below. The audit at
//! pickup time (against `adapters/streamlib-adapter-vulkan/src/`)
//! enumerated:
//!
//! 1. `SurfaceAdapter` trait methods (from
//!    `streamlib-adapter-abi`) implemented on `VulkanSurfaceAdapter`:
//!    `acquire_read`, `acquire_write`, `try_acquire_read`,
//!    `try_acquire_write`, `end_read_access`, `end_write_access`.
//! 2. Inherent methods on `VulkanSurfaceAdapter<D>` /
//!    `VulkanContext<D>`: `register_host_surface`,
//!    `unregister_host_surface`, `release_to_foreign`,
//!    `surface_image_info`, `registered_count`, `raw_handles`.
//! 3. RAII guard `Drop` paths (`ReadGuard::drop` /
//!    `WriteGuard::drop`) route back through
//!    `SurfaceAdapter::end_*_access` — covered by the same vtable
//!    slots.
//! 4. View capability accessors (`VulkanWritable::vk_image`,
//!    `vk_image_layout`, `VulkanImageInfoExt::vk_image_info`) are
//!    pure reads against the [`VulkanViewRepr`] payload returned
//!    by `acquire_*`. No vtable hop on the view itself.
//!
//! Power-user `with_acquire_timeout` (chaining-builder, sets
//! `Duration` at construction) is host-side only and not
//! cdylib-callable; an `Arc<VulkanSurfaceAdapter<_>>` already
//! configured with the desired timeout is what crosses the vtable
//! handle. No slot is required.

#![no_std]

use core::ffi::c_void;

// ============================================================================
// Layout version constants
// ============================================================================

/// Layout version of [`VulkanSurfaceAdapterVTable`].
///
/// Pinned at offset 0 forever; new methods append to the end and
/// bump this constant. Host wiring asserts equality at install
/// time; cdylib code reads it before dereferencing any slot and
/// refuses to proceed on mismatch.
///
/// - v1: trunk lift — handle lifetime (`clone_handle` /
///   `drop_handle`) + 12 method slots covering the full
///   cdylib-callable surface (audit above). Locked by
///   [`tests::vulkan_surface_adapter_vtable_layout`] and the
///   tier-1 null-handle tests next to it.
pub const VULKAN_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION: u32 = 1;

// ============================================================================
// View payload — pure data the host writes into a caller-provided slot
// ============================================================================

/// Vulkan `VkImageLayout` enumerant value.
///
/// Mirrors `streamlib_adapter_abi::VkImageLayoutValue` as a
/// dependency-free repeat so this crate stays free of streamlib
/// deps. `VkImageLayout` is a 32-bit signed enum per the Vulkan
/// spec; the value is interpreted by the host's `vulkanalia`
/// binding (or any other Vulkan binding) on either side of the
/// ABI.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct VkImageLayoutValueRepr(pub i32);

/// `#[repr(C)]` mirror of `streamlib_adapter_abi::VkImageInfo`.
///
/// Layout MUST match `VkImageInfo` byte-for-byte — the host
/// implementation populates this struct directly from the existing
/// adapter and the cdylib reads the same offsets through its
/// PluginAbiObject view payload. Adding fields requires a coordinated bump
/// in both this crate AND `streamlib-adapter-abi`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VkImageInfoRepr {
    /// `VkFormat` enumerant. `i32` per spec.
    pub format: i32,
    /// `VkImageTiling` enumerant. `i32` per spec.
    pub tiling: i32,
    /// `VkImageUsageFlags` bitmask.
    pub usage_flags: u32,
    /// `VkSampleCountFlagBits` bitmask (1 = `VK_SAMPLE_COUNT_1_BIT`).
    pub sample_count: u32,
    /// Number of mip levels.
    pub level_count: u32,
    /// Owning `VkQueue` family index.
    pub queue_family: u32,
    /// Opaque `VkDeviceMemory` handle.
    pub memory_handle: u64,
    /// Byte offset of the image within `memory_handle`.
    pub memory_offset: u64,
    /// Byte size of the image's region within `memory_handle`.
    pub memory_size: u64,
    /// `VkMemoryPropertyFlags` bitmask of the backing allocation.
    pub memory_property_flags: u32,
    /// 1 if the image was allocated `VK_IMAGE_CREATE_PROTECTED_BIT`.
    pub protected: u32,
    /// Opaque `VkSamplerYcbcrConversion` handle, or 0 if unused.
    pub ycbcr_conversion: u64,
    /// Reserved bytes for additive ABI extensions. MUST be zeroed.
    pub _reserved: [u8; 16],
}

/// `#[repr(C)]` payload returned by `acquire_read` / `acquire_write` /
/// `try_acquire_*`.
///
/// Carries the live `VkImage` handle, the post-transition layout
/// the adapter left the image in, and the per-image
/// [`VkImageInfoRepr`] descriptor. The cdylib's `VulkanReadView`
/// / `VulkanWriteView` PluginAbiObjects deref these fields directly as
/// POD reads (mirrors the cached-fields pattern on `Texture` /
/// `PixelBuffer` from `streamlib-plugin-abi`).
///
/// Lifetime: valid for the duration of the matching acquire scope
/// — the host's underlying `VulkanReadView<'g>` /
/// `VulkanWriteView<'g>` keeps the `VkImage` alive via the
/// adapter's registry until `end_*_access` is called.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VulkanViewRepr {
    /// Opaque `VkImage` handle (Vulkan handles are 64-bit per spec).
    pub vk_image: u64,
    /// `VkImageLayout` the adapter transitioned the image into.
    pub vk_image_layout: VkImageLayoutValueRepr,
    /// Padding so [`Self::info`] is naturally aligned. Zero today,
    /// never read.
    pub _padding: u32,
    /// Full image descriptor for compositors that need to wrap the
    /// underlying `VkImage` as a framework-native render target
    /// (Skia's `GrVkImageInfo` shape).
    pub info: VkImageInfoRepr,
}

/// `#[repr(C)]` mirror of `streamlib_adapter_vulkan::RawVulkanHandles`.
///
/// Power-user surface — every field is the raw Vulkan handle bits
/// (`as_raw()`) so the cdylib can reconstruct the typed wrapper
/// its binding wants (`ash::Image::from_raw`,
/// `vulkanalia::vk::Image::from_raw`, custom FFI shims).
///
/// Layout MUST match the in-crate `RawVulkanHandles` byte-for-byte
/// — the host populates this via `raw_handles()`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RawVulkanHandlesRepr {
    /// `VkInstance` handle.
    pub vk_instance: u64,
    /// `VkPhysicalDevice` handle.
    pub vk_physical_device: u64,
    /// `VkDevice` handle.
    pub vk_device: u64,
    /// `VkQueue` of the graphics-and-present queue family. Caller
    /// MUST take the per-queue mutex if they intend to submit
    /// work — the streamlib RHI does this internally; raw users
    /// assume the responsibility.
    pub vk_queue: u64,
    /// Queue family index for `vk_queue`. Used in
    /// `VkImageMemoryBarrier::srcQueueFamilyIndex` /
    /// `dstQueueFamilyIndex` for cross-queue ownership transitions.
    pub vk_queue_family_index: u32,
    /// Vulkan API version the streamlib runtime requested at
    /// instance creation. Encoded per `VK_MAKE_API_VERSION`.
    pub api_version: u32,
}

// ============================================================================
// VulkanSurfaceAdapterVTable
// ============================================================================

/// Dispatch table for the host's `VulkanSurfaceAdapter<D>`.
///
/// The cdylib holds an opaque `*const c_void` handle (an
/// `Arc::into_raw(Arc<VulkanSurfaceAdapter<D>>)`-shaped pointer
/// produced by the host) plus a `*const VulkanSurfaceAdapterVTable`
/// it reads from the `HostServices` payload when the cdylib PluginAbiObject
/// lift lands (sibling slice to this trunk PR). Method-dispatch
/// callbacks cover every cdylib-callable inherent method on
/// `VulkanSurfaceAdapter` plus the `SurfaceAdapter` trait methods.
///
/// # Handle lifetime
///
/// `clone_handle` / `drop_handle` mirror the
/// `GpuContextLimitedAccessVTable` v2 pattern from
/// `streamlib-plugin-abi`: `clone_handle(borrowed) -> owned` bumps
/// the host's `Arc<VulkanSurfaceAdapter<D>>` refcount;
/// `drop_handle(owned)` releases. The owned handle remains valid
/// even after the originating runtime context is dropped, which
/// matches the existing `VulkanContext: Clone` contract — a plugin
/// can stash a clone in `setup()` and hand it to a worker thread
/// that outlives the lifecycle call.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. New methods
/// append to the end and bump
/// [`VULKAN_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
///
/// # Error crossing
///
/// Every fallible slot returns `i32` (0 = success, non-zero =
/// error). On error the host writes a UTF-8 message into the
/// caller-provided `err_buf` (clamped to `err_buf_cap`) and sets
/// `*err_len` to the bytes written. The wire shape mirrors the
/// host-side error-buffer convention already established by
/// `GpuContextLimitedAccessVTable::acquire_texture` and siblings;
/// truncation never trips a panic.
///
/// Non-error sentinels: `try_acquire_*` distinguishes "contended,
/// retry later" (status = 0, `*out_acquired = 0`) from "real
/// failure" (status = 1, error written) so the host's `Ok(None)`
/// vs `Err(...)` distinction survives the i32 crossing.
#[repr(C)]
pub struct VulkanSurfaceAdapterVTable {
    /// Vtable layout version. Must equal
    /// [`VULKAN_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    // -----------------------------------------------------------------
    // Handle lifetime
    // -----------------------------------------------------------------
    /// Take a borrowed handle (typically minted by the host's
    /// runtime context when wiring the cdylib-side `VulkanContext`
    /// PluginAbiObject) and return a new owned handle with an Arc refcount
    /// bump on the underlying `Arc<VulkanSurfaceAdapter<D>>`. The
    /// owned handle remains valid even after the originating
    /// context is dropped and MUST be released exactly once via
    /// [`Self::drop_handle`]. Calling on a null pointer returns
    /// null.
    pub clone_handle: unsafe extern "C" fn(borrowed_handle: *const c_void) -> *const c_void,

    /// Release an owned handle previously obtained from
    /// [`Self::clone_handle`]. Calling on a null pointer is a
    /// no-op. Calling on the same owned handle twice is undefined
    /// behaviour (double-free of the Arc refcount).
    pub drop_handle: unsafe extern "C" fn(owned_handle: *const c_void),

    // -----------------------------------------------------------------
    // Registry management (inherent on VulkanSurfaceAdapter)
    // -----------------------------------------------------------------
    /// Register a surface with the adapter.
    ///
    /// `texture_handle` is an
    /// `Arc::into_raw(Arc<<D::Privilege as DevicePrivilege>::Texture>)`-shaped
    /// opaque pointer produced by the host; the host implementation
    /// bumps the Arc refcount (`Arc::increment_strong_count`) and
    /// stores a clone in the adapter's internal registry. The
    /// caller's `texture_handle` Arc remains owned by the caller.
    ///
    /// `produce_done_handle` and `consume_done_handle` are
    /// `Arc::into_raw(Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore>)`-shaped
    /// opaque pointers with identical ownership semantics. They are
    /// the two single-writer-per-edge timeline semaphores documented
    /// in `docs/architecture/adapter-timeline-single-writer.md`:
    /// `produce_done` is signaled exclusively by the producer
    /// process (from `end_write_access`), `consume_done` exclusively
    /// by the consumer process (from `end_read_access`).
    ///
    /// `initial_layout_raw` is the i32 `VkImageLayout` enumerant
    /// the texture is in at registration time.
    ///
    /// Returns 0 on success, non-zero on failure (e.g. surface_id
    /// already registered). On error writes a UTF-8 message into
    /// `err_buf`.
    pub register_host_surface: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id: u64,
        texture_handle: *const c_void,
        produce_done_handle: *const c_void,
        consume_done_handle: *const c_void,
        initial_layout_raw: i32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Drop a registered surface from the adapter. Idempotent —
    /// missing entries return 0 via `*out_was_present = 0`. Calls
    /// against a null handle return 0 with `*out_was_present = 0`.
    pub unregister_host_surface:
        unsafe extern "C" fn(handle: *const c_void, surface_id: u64, out_was_present: *mut u32),

    /// Snapshot the adapter's registry size (number of currently-
    /// registered surfaces). Returns 0 on null handle. Used for
    /// host-side tests and observability; cdylibs can call it
    /// today but the read is informational, not synchronizing.
    pub registered_count: unsafe extern "C" fn(handle: *const c_void) -> usize,

    // -----------------------------------------------------------------
    // SurfaceAdapter trait methods
    // -----------------------------------------------------------------
    /// Blocking read acquire.
    ///
    /// `surface_ptr` is a `*const StreamlibSurface` borrowed from
    /// the caller's stack; valid for the duration of the call. On
    /// success writes the populated [`VulkanViewRepr`] into
    /// `*out_view` and returns 0. On contention (Ok(None) shape)
    /// the host returns 1 with a "writer contended" message; the
    /// blocking variant never returns the contended-but-not-error
    /// case (`try_acquire_*` does).
    pub acquire_read: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut VulkanViewRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Blocking write acquire. Same shape as [`Self::acquire_read`]
    /// but exclusive-write semantics.
    pub acquire_write: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut VulkanViewRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Non-blocking read acquire.
    ///
    /// Returns 0 on success and writes `*out_acquired = 1` plus a
    /// populated view. Returns 0 with `*out_acquired = 0` on
    /// contention (Ok(None) shape) without writing the view.
    /// Returns non-zero with an error message on real failure.
    pub try_acquire_read: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut VulkanViewRepr,
        out_acquired: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Non-blocking write acquire. Same shape as
    /// [`Self::try_acquire_read`].
    pub try_acquire_write: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut VulkanViewRepr,
        out_acquired: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Sealed: signal the release-side timeline semaphore for a
    /// read. Called by the cdylib's `ReadGuard::drop`. Idempotent
    /// against unknown surface IDs (the host logs and returns; no
    /// error surfaced because Drop can't propagate).
    pub end_read_access: unsafe extern "C" fn(handle: *const c_void, surface_id: u64),

    /// Sealed: signal the release-side timeline semaphore for a
    /// write. Called by the cdylib's `WriteGuard::drop`.
    pub end_write_access: unsafe extern "C" fn(handle: *const c_void, surface_id: u64),

    // -----------------------------------------------------------------
    // Cross-process publishing helpers
    // -----------------------------------------------------------------
    /// Producer-side QFOT release for cross-process publishing.
    /// Wraps [`streamlib_adapter_vulkan::VulkanSurfaceAdapter::release_to_foreign`].
    ///
    /// On success writes the resulting layout into
    /// `*out_resulting_layout_raw` and returns 0. On failure
    /// writes a UTF-8 message into `err_buf` and returns non-zero.
    pub release_to_foreign: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id: u64,
        post_release_layout_raw: i32,
        out_resulting_layout_raw: *mut i32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -----------------------------------------------------------------
    // Per-surface descriptor accessors
    // -----------------------------------------------------------------
    /// Resolve the full [`VkImageInfoRepr`] descriptor for a
    /// registered surface. Sets `*out_found = 1` and writes the
    /// descriptor on hit; `*out_found = 0` on miss (in which case
    /// `*out_info` is left untouched). Calling on a null handle
    /// sets `*out_found = 0`.
    pub surface_image_info: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id: u64,
        out_info: *mut VkImageInfoRepr,
        out_found: *mut u32,
    ),

    // -----------------------------------------------------------------
    // Power-user raw handles
    // -----------------------------------------------------------------
    /// Snapshot the underlying device's raw Vulkan handles. The
    /// handles are valid for the lifetime of the device; the
    /// caller MUST NOT outlive the runtime that owns it. There is
    /// intentionally no destructor or refcount — power-user
    /// "you own the consequences" surface. Calling on a null
    /// handle writes a zeroed [`RawVulkanHandlesRepr`].
    pub raw_handles:
        unsafe extern "C" fn(handle: *const c_void, out_handles: *mut RawVulkanHandlesRepr),
}

// Safety: every field is a primitive integer or an `unsafe extern "C" fn`
// pointer; no thread-local state, no interior mutability. The host
// guarantees the pointed-at adapter state outlives every cdylib that
// holds a clone of the handle via the loader's pinning shape.
unsafe impl Send for VulkanSurfaceAdapterVTable {}
unsafe impl Sync for VulkanSurfaceAdapterVTable {}

// ============================================================================
// Layout regression tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    /// `VkImageInfoRepr` mirrors `streamlib_adapter_abi::VkImageInfo`
    /// byte-for-byte. Locks the contract one of the four plugin ABI
    /// payloads in this crate rides on.
    #[test]
    fn vk_image_info_repr_layout() {
        // format: i32 @ 0
        // tiling: i32 @ 4
        // usage_flags: u32 @ 8
        // sample_count: u32 @ 12
        // level_count: u32 @ 16
        // queue_family: u32 @ 20
        // memory_handle: u64 @ 24
        // memory_offset: u64 @ 32
        // memory_size: u64 @ 40
        // memory_property_flags: u32 @ 48
        // protected: u32 @ 52
        // ycbcr_conversion: u64 @ 56
        // _reserved: [u8; 16] @ 64
        // total: 80 bytes, align 8
        assert_eq!(offset_of!(VkImageInfoRepr, format), 0);
        assert_eq!(offset_of!(VkImageInfoRepr, tiling), 4);
        assert_eq!(offset_of!(VkImageInfoRepr, usage_flags), 8);
        assert_eq!(offset_of!(VkImageInfoRepr, sample_count), 12);
        assert_eq!(offset_of!(VkImageInfoRepr, level_count), 16);
        assert_eq!(offset_of!(VkImageInfoRepr, queue_family), 20);
        assert_eq!(offset_of!(VkImageInfoRepr, memory_handle), 24);
        assert_eq!(offset_of!(VkImageInfoRepr, memory_offset), 32);
        assert_eq!(offset_of!(VkImageInfoRepr, memory_size), 40);
        assert_eq!(offset_of!(VkImageInfoRepr, memory_property_flags), 48);
        assert_eq!(offset_of!(VkImageInfoRepr, protected), 52);
        assert_eq!(offset_of!(VkImageInfoRepr, ycbcr_conversion), 56);
        assert_eq!(offset_of!(VkImageInfoRepr, _reserved), 64);
        assert_eq!(size_of::<VkImageInfoRepr>(), 80);
        assert_eq!(align_of::<VkImageInfoRepr>(), 8);
    }

    /// `VulkanViewRepr` carries (`vk_image: u64`,
    /// `vk_image_layout: i32`, padding `u32`, `info:
    /// VkImageInfoRepr`). Padding lands the `info` block on a
    /// natural 8-byte boundary.
    #[test]
    fn vulkan_view_repr_layout() {
        // vk_image: u64 @ 0
        // vk_image_layout: i32 @ 8 (VkImageLayoutValueRepr is #[repr(transparent)] i32)
        // _padding: u32 @ 12
        // info: VkImageInfoRepr (80 B, align 8) @ 16
        // total: 96 bytes, align 8
        assert_eq!(offset_of!(VulkanViewRepr, vk_image), 0);
        assert_eq!(offset_of!(VulkanViewRepr, vk_image_layout), 8);
        assert_eq!(offset_of!(VulkanViewRepr, _padding), 12);
        assert_eq!(offset_of!(VulkanViewRepr, info), 16);
        assert_eq!(size_of::<VulkanViewRepr>(), 96);
        assert_eq!(align_of::<VulkanViewRepr>(), 8);
    }

    #[test]
    fn vk_image_layout_value_repr_is_transparent_i32() {
        assert_eq!(size_of::<VkImageLayoutValueRepr>(), 4);
        assert_eq!(align_of::<VkImageLayoutValueRepr>(), 4);
    }

    /// `RawVulkanHandlesRepr` matches `RawVulkanHandles` —
    /// `u64×4, u32×2` = 32 + 8 = 40 bytes, align 8.
    #[test]
    fn raw_vulkan_handles_repr_layout() {
        assert_eq!(offset_of!(RawVulkanHandlesRepr, vk_instance), 0);
        assert_eq!(offset_of!(RawVulkanHandlesRepr, vk_physical_device), 8);
        assert_eq!(offset_of!(RawVulkanHandlesRepr, vk_device), 16);
        assert_eq!(offset_of!(RawVulkanHandlesRepr, vk_queue), 24);
        assert_eq!(offset_of!(RawVulkanHandlesRepr, vk_queue_family_index), 32);
        assert_eq!(offset_of!(RawVulkanHandlesRepr, api_version), 36);
        assert_eq!(size_of::<RawVulkanHandlesRepr>(), 40);
        assert_eq!(align_of::<RawVulkanHandlesRepr>(), 8);
    }

    /// Locks the vtable's binary layout. Anchors every method slot
    /// at a fixed byte offset so a host built against vtable v1
    /// and a cdylib built against vtable v1 dispatch through the
    /// same offsets regardless of rustc-minor / dep-graph drift.
    /// New methods must append after `raw_handles` and bump
    /// [`VULKAN_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
    #[test]
    fn vulkan_surface_adapter_vtable_layout() {
        // layout_version: u32 @ 0
        // _reserved_padding: u32 @ 4
        // 14 fn pointers (8 bytes each) @ 8..120
        // total: 4 + 4 + 14*8 = 120 bytes, align 8
        assert_eq!(VULKAN_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(size_of::<VulkanSurfaceAdapterVTable>(), 120);
        assert_eq!(align_of::<VulkanSurfaceAdapterVTable>(), 8);
        assert_eq!(offset_of!(VulkanSurfaceAdapterVTable, layout_version), 0);
        assert_eq!(offset_of!(VulkanSurfaceAdapterVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(VulkanSurfaceAdapterVTable, clone_handle), 8);
        assert_eq!(offset_of!(VulkanSurfaceAdapterVTable, drop_handle), 16);
        assert_eq!(
            offset_of!(VulkanSurfaceAdapterVTable, register_host_surface),
            24
        );
        assert_eq!(
            offset_of!(VulkanSurfaceAdapterVTable, unregister_host_surface),
            32
        );
        assert_eq!(offset_of!(VulkanSurfaceAdapterVTable, registered_count), 40);
        assert_eq!(offset_of!(VulkanSurfaceAdapterVTable, acquire_read), 48);
        assert_eq!(offset_of!(VulkanSurfaceAdapterVTable, acquire_write), 56);
        assert_eq!(offset_of!(VulkanSurfaceAdapterVTable, try_acquire_read), 64);
        assert_eq!(
            offset_of!(VulkanSurfaceAdapterVTable, try_acquire_write),
            72
        );
        assert_eq!(offset_of!(VulkanSurfaceAdapterVTable, end_read_access), 80);
        assert_eq!(offset_of!(VulkanSurfaceAdapterVTable, end_write_access), 88);
        assert_eq!(
            offset_of!(VulkanSurfaceAdapterVTable, release_to_foreign),
            96
        );
        assert_eq!(
            offset_of!(VulkanSurfaceAdapterVTable, surface_image_info),
            104
        );
        assert_eq!(offset_of!(VulkanSurfaceAdapterVTable, raw_handles), 112);
    }

    /// Compile-time witness that the vtable is `Send + Sync`. A
    /// future regression that adds an interior-mutable or non-thread-
    /// safe field would break the unsafe impl above and trip this
    /// test at build time.
    #[test]
    fn vtable_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<VulkanSurfaceAdapterVTable>();
    }
}
