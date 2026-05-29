// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Plugin ABI callback table for `streamlib-adapter-cpu-readback`.
//!
//! Sibling of [`streamlib-adapter-vulkan-abi`]'s
//! `VulkanSurfaceAdapterVTable` (issue #887) — same shape applied to
//! the explicit GPU→CPU readback adapter. Audit of the cdylib-callable
//! surface is enumerated below.
//!
//! # What this crate is
//!
//! A pure ABI contract describing how a host exposes its
//! `CpuReadbackSurfaceAdapter<D>` to a cdylib plugin without sharing
//! any Rust types beyond `#[repr(C)]` payloads and
//! `unsafe extern "C" fn` pointers. The cdylib carries a
//! `(handle, vtable)` PluginAbiObject; the host dispatches every method
//! through host-compiled code so layout drift between rustc-minor
//! versions and divergent dep graphs is contained inside the host
//! plugin.
//!
//! Dep posture mirrors [`streamlib-plugin-abi`] /
//! [`streamlib-adapter-vulkan-abi`]: zero streamlib crates pulled,
//! zero vulkanalia, zero rustc-version-coupled types.
//!
//! # Audited cdylib-callable surface
//!
//! Every method on the cdylib-facing side of
//! `CpuReadbackSurfaceAdapter<D>` / `CpuReadbackContext<D>` / its
//! acquire guards is covered by exactly one slot below. The audit
//! at pickup time (against
//! `libs/streamlib-adapter-cpu-readback/src/`) enumerated:
//!
//! 1. `SurfaceAdapter` trait methods (from
//!    `streamlib-adapter-abi`) implemented on
//!    `CpuReadbackSurfaceAdapter`: `acquire_read`, `acquire_write`,
//!    `try_acquire_read`, `try_acquire_write`, `end_read_access`,
//!    `end_write_access`.
//! 2. Inherent methods on `CpuReadbackSurfaceAdapter<D>`:
//!    `register_host_surface`, `unregister_host_surface`,
//!    `registered_count`, `run_bridge_copy_image_to_buffer`,
//!    `run_bridge_copy_buffer_to_image`.
//! 3. RAII guard `Drop` paths (`ReadGuard::drop` /
//!    `WriteGuard::drop`) route back through
//!    `SurfaceAdapter::end_*_access` — covered by the same vtable
//!    slots.
//! 4. View capability accessors
//!    (`CpuReadbackPlaneView::bytes` / `width` / `height` /
//!    `bytes_per_pixel`, `CpuReadbackReadView::format` /
//!    `plane_count` / `planes`) are pure POD reads against the
//!    [`CpuReadbackViewRepr`] payload returned by `acquire_*`. No
//!    vtable hop on the view itself.
//!
//! Methods deliberately not exposed (audit findings):
//!
//! - `CpuReadbackSurfaceAdapter::new` / `with_acquire_timeout` —
//!   host-side construction / chaining-builder; cdylib receives a
//!   vtable+handle, never constructs adapters.
//! - `device()` — returns `&Arc<D>`, can't cross extern "C" without
//!   `D` parameterization; cdylibs hold their own device.
//! - `InProcessCpuReadbackCopyTrigger` / `CpuReadbackCopyTrigger` —
//!   trigger flavor is selected at adapter construction (host wires
//!   the in-process trigger; cdylibs wire an escalate-IPC trigger).
//!   The trigger trait is private wiring inside the adapter, not a
//!   cdylib-callable boundary.
//!
//! # Scope fence — escalate-IPC seam is out of scope here
//!
//! The cpu-readback adapter is special: per-acquire GPU work runs on
//! the host via a thin `run_cpu_readback_copy(surface_id)` escalate-
//! IPC trigger (see
//! `docs/architecture/adapter-runtime-integration.md`). That trigger
//! is **already cross-process** through escalate IPC + the host's
//! `CpuReadbackBridge` trait on `GpuContext`. This vtable is for
//! cdylib-resident processors that hold a CpuReadback-flavor adapter
//! context (`Arc<CpuReadbackSurfaceAdapter<ConsumerVulkanDevice>>`)
//! and call `adapter.acquire_read(surface)` directly — NOT for the
//! host-side bridge. The bridge stays where it lives; this vtable
//! covers the parallel "cdylib holds the adapter, talks to its own
//! trigger" surface only.
//!
//! # Multi-plane views
//!
//! Unlike the vulkan adapter (whose view is a single `VkImage` +
//! layout), cpu-readback's read/write views are multi-plane:
//! single-plane for BGRA/RGBA (one tightly-packed staging buffer)
//! and two-plane for NV12 (Y + UV). The wire shape carries up to
//! [`MAX_PLANES`] plane slots in a fixed-size array plus an
//! explicit `plane_count`; current in-tree formats need at most 2.
//! Future YUV variants (NV21, I420, I422, I444) fit within
//! `MAX_PLANES = 4` without bumping the layout version.

#![no_std]

use core::ffi::c_void;

// ============================================================================
// Layout version constants
// ============================================================================

/// Layout version of [`CpuReadbackSurfaceAdapterVTable`].
///
/// Pinned at offset 0 forever; new methods append to the end and
/// bump this constant. Host wiring asserts equality at install time;
/// cdylib code reads it before dereferencing any slot and refuses to
/// proceed on mismatch.
///
/// - v1: trunk lift — handle lifetime (`clone_handle` /
///   `drop_handle`) + 11 method slots covering the full
///   cdylib-callable surface (audit above). Locked by
///   [`tests::cpu_readback_surface_adapter_vtable_layout`] and the
///   tier-1 null-handle tests in the host wiring crate.
pub const CPU_READBACK_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Maximum number of planes carried in [`CpuReadbackViewRepr`] and
/// [`HostSurfaceRegistrationRepr`].
///
/// Today's `SurfaceFormat` values use at most 2 (NV12 Y + UV). 4
/// provides headroom for the common forward-looking YUV variants
/// (NV21, I420, I422, I444 — all ≤ 3 planes) without bumping the
/// layout version when they land. A future format that needs more
/// than 4 planes triggers a layout-version bump (the array grows
/// and the size of [`CpuReadbackViewRepr`] /
/// [`HostSurfaceRegistrationRepr`] changes; layout regression
/// tests pin the count so a stealth bump trips at compile time).
pub const MAX_PLANES: usize = 4;

// ============================================================================
// View payload — pure data the host writes into a caller-provided slot
// ============================================================================

/// Per-plane geometry + raw mapped pointer.
///
/// Mirrors the shape `CpuReadbackPlaneView` /
/// `CpuReadbackPlaneViewMut` expose to in-process callers, flattened
/// into a `#[repr(C)]` record. The `mapped_ptr` is the host's
/// staging-buffer `vk::DeviceMemory` mapped address — for cpu-
/// readback this is HOST_VISIBLE / HOST_COHERENT, so the cdylib
/// reads / writes the bytes directly without any further plugin ABI hop.
/// `byte_size` is `width * height * bytes_per_pixel`, the tightly-
/// packed plane byte count.
///
/// Layout: `u64 + u64 + u32 + u32 + u32 + u32` = 32 bytes, align 8.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CpuReadbackPlaneRepr {
    /// Host-side mapped pointer for this plane's staging buffer.
    /// Carried as `u64` (target-triple-stable) rather than
    /// `*mut u8` so the wire shape is identical on every Linux
    /// platform and the layout regression test doesn't depend on
    /// pointer width.
    pub mapped_ptr: u64,
    /// Tightly-packed plane size in bytes (`width * height *
    /// bytes_per_pixel`).
    pub byte_size: u64,
    /// Plane width in texels.
    pub width: u32,
    /// Plane height in texels.
    pub height: u32,
    /// Bytes per texel of this plane. BGRA/RGBA: 4, NV12 Y: 1,
    /// NV12 UV: 2.
    pub bytes_per_pixel: u32,
    /// Reserved padding. MUST be zeroed.
    pub _padding: u32,
}

impl CpuReadbackPlaneRepr {
    /// Construct a zero-initialised plane repr (mapped pointer
    /// null, geometry zero). Used to pre-populate the
    /// `[CpuReadbackPlaneRepr; MAX_PLANES]` slot before passing it
    /// to an `acquire_*` callback.
    pub const fn zeroed() -> Self {
        Self {
            mapped_ptr: 0,
            byte_size: 0,
            width: 0,
            height: 0,
            bytes_per_pixel: 0,
            _padding: 0,
        }
    }
}

/// `#[repr(C)]` payload returned by `acquire_read` / `acquire_write` /
/// `try_acquire_*`.
///
/// Carries the surface-level format + dimensions and an
/// `[CpuReadbackPlaneRepr; MAX_PLANES]` array of per-plane records,
/// with the live plane count in `plane_count`. Slots beyond
/// `plane_count` MUST be left zeroed by the host. The cdylib's
/// `CpuReadbackReadView` / `CpuReadbackWriteView` PluginAbiObjects deref
/// these fields directly as POD reads.
///
/// Lifetime: valid for the duration of the matching acquire scope —
/// the host's underlying `CpuReadbackReadView<'g>` /
/// `CpuReadbackWriteView<'g>` keeps the staging buffers' mapped
/// pointers alive via the adapter's registry until `end_*_access`
/// is called and the timeline release completes.
///
/// Layout: `u32×4 + [CpuReadbackPlaneRepr; 4] @ 16` =
/// `16 + 128` = 144 bytes, align 8.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CpuReadbackViewRepr {
    /// `SurfaceFormat` enumerant value
    /// (`Bgra8 = 0` / `Rgba8 = 1` / `Nv12 = 2`). Mirrors
    /// `streamlib_adapter_abi::SurfaceFormat`'s `#[repr(u32)]`
    /// representation byte-for-byte.
    pub format_raw: u32,
    /// Surface width in pixels (= plane 0 width).
    pub width: u32,
    /// Surface height in pixels (= plane 0 height).
    pub height: u32,
    /// Number of populated entries in [`Self::planes`]. Slots
    /// `[plane_count..MAX_PLANES]` are zeroed.
    pub plane_count: u32,
    /// Per-plane geometry + mapped pointer. Logical layout
    /// matches the format's planes in declaration order — NV12:
    /// `[Y, UV, zeroed, zeroed]`.
    pub planes: [CpuReadbackPlaneRepr; MAX_PLANES],
}

impl CpuReadbackViewRepr {
    /// Construct a zero-initialised view repr. Used by callers to
    /// pre-populate an `out_view` slot before passing it to an
    /// `acquire_*` callback.
    pub const fn zeroed() -> Self {
        Self {
            format_raw: 0,
            width: 0,
            height: 0,
            plane_count: 0,
            planes: [CpuReadbackPlaneRepr::zeroed(); MAX_PLANES],
        }
    }
}

// ============================================================================
// HostSurfaceRegistration payload — registration metadata + handle bundle
// ============================================================================

/// `#[repr(C)]` mirror of `HostSurfaceRegistration<P>` from
/// `streamlib-adapter-cpu-readback::state`.
///
/// Carries the per-surface registration metadata plus the bundle of
/// `Arc::into_raw`-shaped opaque handles the host needs to bump
/// refcount: a (nullable) source-texture handle, a timeline handle,
/// and an array of staging-buffer handles (one per plane). The host
/// implementation increments the refcount on each non-null handle
/// and stashes a clone in the registry; the caller's Arcs remain
/// owned by the caller.
///
/// The handle field layout (`u64` for `*const c_void`-shaped
/// pointers, target-triple-stable on every Linux platform) keeps
/// the wire shape identical regardless of pointer width tracking;
/// callers cast `*const c_void` to `u64` via `as u64` and back via
/// `as *const c_void` on dispatch.
///
/// Layout: `u32×4 + i32 + u32 + u64×6` =
/// `4 + 4 + 4 + 4 + 4 + 4 + 8 + 8 + 32` = 72 bytes, align 8.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HostSurfaceRegistrationRepr {
    /// `SurfaceFormat` enumerant value (mirror of
    /// `SurfaceFormat::Bgra8 = 0` etc).
    pub format_raw: u32,
    /// Surface width in pixels.
    pub width: u32,
    /// Surface height in pixels.
    pub height: u32,
    /// Number of populated entries in [`Self::staging_handles`].
    /// MUST equal the format's natural plane count
    /// (Bgra8/Rgba8 → 1, Nv12 → 2). Slots
    /// `[plane_count..MAX_PLANES]` MUST be 0.
    pub plane_count: u32,
    /// Initial `VkImageLayout` enumerant of the source texture at
    /// registration time. `VK_IMAGE_LAYOUT_UNDEFINED` (0) for
    /// freshly-allocated images.
    pub initial_layout_raw: i32,
    /// Reserved padding. MUST be zeroed.
    pub _padding: u32,
    /// `Arc::into_raw(Arc<<D::Privilege as DevicePrivilege>::Texture>)`-shaped
    /// opaque handle for the source `VkImage`, or `0` if the
    /// registration does not provide one. Consumer-flavor
    /// registrations typically pass 0 (the consumer-side adapter
    /// cannot reach the host's `VkImage` and never issues image
    /// transitions itself; only the host-side trigger does, and
    /// only on host-flavor registrations).
    pub texture_handle: u64,
    /// `Arc::into_raw(Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore>)`-shaped
    /// opaque handle for the `produce_done` timeline (the producer
    /// process signals it via the trigger's GPU submit). MUST be
    /// non-zero. Single-writer-per-edge per
    /// `docs/architecture/adapter-timeline-single-writer.md`.
    pub produce_done_handle: u64,
    /// `Arc::into_raw(Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore>)`-shaped
    /// opaque handle for the `consume_done` timeline (the consumer
    /// process signals it from `end_read_access`). MUST be non-zero.
    pub consume_done_handle: u64,
    /// `Arc::into_raw(Arc<<D::Privilege as DevicePrivilege>::Buffer>)`-shaped
    /// opaque handles for each per-plane staging buffer. Logical
    /// layout matches the format's planes in declaration order.
    /// Slots `[plane_count..MAX_PLANES]` MUST be 0.
    pub staging_handles: [u64; MAX_PLANES],
}

impl HostSurfaceRegistrationRepr {
    /// Construct a zero-initialised registration repr. Callers
    /// populate `format_raw`, `width`, `height`, `plane_count`,
    /// `initial_layout_raw`, `produce_done_handle`,
    /// `consume_done_handle`, and the leading `plane_count` entries
    /// of `staging_handles` before dispatch.
    pub const fn zeroed() -> Self {
        Self {
            format_raw: 0,
            width: 0,
            height: 0,
            plane_count: 0,
            initial_layout_raw: 0,
            _padding: 0,
            texture_handle: 0,
            produce_done_handle: 0,
            consume_done_handle: 0,
            staging_handles: [0; MAX_PLANES],
        }
    }
}

// ============================================================================
// CpuReadbackSurfaceAdapterVTable
// ============================================================================

/// Dispatch table for the host's `CpuReadbackSurfaceAdapter<D>`.
///
/// The cdylib holds an opaque `*const c_void` handle (an
/// `Arc::into_raw(Arc<CpuReadbackSurfaceAdapter<D>>)`-shaped pointer
/// produced by the host) plus a
/// `*const CpuReadbackSurfaceAdapterVTable` it reads from the
/// `HostServices` payload when the cdylib PluginAbiObject lift lands
/// (sibling slice to this trunk PR — see scope note below).
/// Method-dispatch callbacks cover every cdylib-callable inherent
/// method on `CpuReadbackSurfaceAdapter` plus the `SurfaceAdapter`
/// trait methods.
///
/// # Handle lifetime
///
/// `clone_handle` / `drop_handle` mirror the
/// [`streamlib-adapter-vulkan-abi`] v1 pattern:
/// `clone_handle(borrowed) -> owned` bumps the host's
/// `Arc<CpuReadbackSurfaceAdapter<D>>` refcount;
/// `drop_handle(owned)` releases. The owned handle remains valid
/// even after the originating runtime context is dropped, which
/// matches the existing `CpuReadbackContext: Clone` contract — a
/// plugin can stash a clone in `setup()` and hand it to a worker
/// thread that outlives the lifecycle call.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. New methods
/// append to the end and bump
/// [`CPU_READBACK_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
///
/// # Error crossing
///
/// Every fallible slot returns `i32` (0 = success, non-zero =
/// error). On error the host writes a UTF-8 message into the
/// caller-provided `err_buf` (clamped to `err_buf_cap`) and sets
/// `*err_len` to the bytes written. The wire shape mirrors the
/// host-side error-buffer convention already established by
/// `streamlib_adapter_vulkan_abi::VulkanSurfaceAdapterVTable`'s
/// fallible slots; truncation never trips a panic.
///
/// Non-error sentinels: `try_acquire_*` distinguishes "contended,
/// retry later" (status = 0, `*out_acquired = 0`) from "real
/// failure" (status = 1, error written) so the host's `Ok(None)`
/// vs `Err(...)` distinction survives the i32 crossing.
#[repr(C)]
pub struct CpuReadbackSurfaceAdapterVTable {
    /// Vtable layout version. Must equal
    /// [`CPU_READBACK_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    // -----------------------------------------------------------------
    // Handle lifetime
    // -----------------------------------------------------------------

    /// Take a borrowed handle (typically minted by the host's
    /// runtime context when wiring the cdylib-side
    /// `CpuReadbackContext` PluginAbiObject) and return a new owned handle
    /// with an Arc refcount bump on the underlying
    /// `Arc<CpuReadbackSurfaceAdapter<D>>`. The owned handle
    /// remains valid even after the originating context is
    /// dropped and MUST be released exactly once via
    /// [`Self::drop_handle`]. Calling on a null pointer returns
    /// null.
    pub clone_handle: unsafe extern "C" fn(borrowed_handle: *const c_void) -> *const c_void,

    /// Release an owned handle previously obtained from
    /// [`Self::clone_handle`]. Calling on a null pointer is a
    /// no-op. Calling on the same owned handle twice is undefined
    /// behaviour (double-free of the Arc refcount).
    pub drop_handle: unsafe extern "C" fn(owned_handle: *const c_void),

    // -----------------------------------------------------------------
    // Registry management (inherent on CpuReadbackSurfaceAdapter)
    // -----------------------------------------------------------------

    /// Register a surface with the adapter.
    ///
    /// `registration_ptr` is a `*const HostSurfaceRegistrationRepr`
    /// borrowed from the caller's stack; valid for the duration of
    /// the call. The host implementation bumps the refcount on
    /// each non-null handle in `registration` (texture if non-zero,
    /// timeline, each staging buffer) and stashes the resulting
    /// Arcs in the adapter's internal registry. The caller's
    /// Arcs remain owned by the caller.
    ///
    /// Returns 0 on success, non-zero on failure (e.g. surface_id
    /// already registered, plane_count mismatch, dim mismatch). On
    /// error writes a UTF-8 message into `err_buf`.
    pub register_host_surface: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id: u64,
        registration_ptr: *const HostSurfaceRegistrationRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Drop a registered surface from the adapter. Idempotent —
    /// missing entries return 0 via `*out_was_present = 0`. Calls
    /// against a null handle return 0 with `*out_was_present = 0`.
    pub unregister_host_surface: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id: u64,
        out_was_present: *mut u32,
    ),

    /// Snapshot the adapter's registry size (number of
    /// currently-registered surfaces). Returns 0 on null handle.
    /// Used for host-side tests and observability; cdylibs can
    /// call it today but the read is informational, not
    /// synchronizing.
    pub registered_count: unsafe extern "C" fn(handle: *const c_void) -> usize,

    // -----------------------------------------------------------------
    // SurfaceAdapter trait methods
    // -----------------------------------------------------------------

    /// Blocking read acquire.
    ///
    /// `surface_ptr` is a `*const StreamlibSurface` borrowed from
    /// the caller's stack; valid for the duration of the call. On
    /// success writes the populated [`CpuReadbackViewRepr`] into
    /// `*out_view` and returns 0. On contention (`Ok(None)` shape)
    /// the host returns 1 with a "writer contended" message; the
    /// blocking variant never returns the contended-but-not-error
    /// case (`try_acquire_*` does).
    pub acquire_read: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut CpuReadbackViewRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Blocking write acquire. Same shape as [`Self::acquire_read`]
    /// but exclusive-write semantics.
    pub acquire_write: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut CpuReadbackViewRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Non-blocking read acquire.
    ///
    /// Returns 0 on success and writes `*out_acquired = 1` plus a
    /// populated view. Returns 0 with `*out_acquired = 0` on
    /// contention (`Ok(None)` shape) without writing the view.
    /// Returns non-zero with an error message on real failure.
    pub try_acquire_read: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut CpuReadbackViewRepr,
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
        out_view: *mut CpuReadbackViewRepr,
        out_acquired: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Sealed: drop the read access counters for a surface. Called
    /// by the cdylib's `ReadGuard::drop`. Idempotent against
    /// unknown surface IDs (the host logs and returns; no error
    /// surfaced because Drop can't propagate).
    pub end_read_access: unsafe extern "C" fn(handle: *const c_void, surface_id: u64),

    /// Sealed: drop the write access counters for a surface AND
    /// run the post-write `vkCmdCopyBufferToImage` flush via the
    /// adapter's trigger. Called by the cdylib's
    /// `WriteGuard::drop`.
    pub end_write_access: unsafe extern "C" fn(handle: *const c_void, surface_id: u64),

    // -----------------------------------------------------------------
    // Bridge entries — direct copy without registry-counter mutation
    // -----------------------------------------------------------------

    /// Bridge entry: run `vkCmdCopyImageToBuffer` for `surface_id`
    /// without going through the in-process registry's
    /// `try_begin_*` / `end_*_access` counters. Used today by the
    /// host-side `CpuReadbackBridge` impl that the escalate
    /// handler reaches when a subprocess sends
    /// `run_cpu_readback_copy(direction=image_to_buffer)`. Cdylib
    /// callers operating against a `ConsumerVulkanDevice`-shaped
    /// adapter will error (the in-process trigger requires a
    /// host-side `VkImage`, which the consumer flavor cannot
    /// provide). Returns 0 on success with the signaled timeline
    /// value in `*out_signaled_value`; non-zero with an error
    /// message on failure.
    pub run_bridge_copy_image_to_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id: u64,
        out_signaled_value: *mut u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bridge entry: run `vkCmdCopyBufferToImage` for `surface_id`.
    /// Mirror of [`Self::run_bridge_copy_image_to_buffer`] — same
    /// semantics, opposite direction.
    pub run_bridge_copy_buffer_to_image: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id: u64,
        out_signaled_value: *mut u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

// Safety: every field is a primitive integer or an `unsafe extern "C" fn`
// pointer; no thread-local state, no interior mutability. The host
// guarantees the pointed-at adapter state outlives every cdylib that
// holds a clone of the handle via the loader's pinning shape.
unsafe impl Send for CpuReadbackSurfaceAdapterVTable {}
unsafe impl Sync for CpuReadbackSurfaceAdapterVTable {}

// ============================================================================
// Layout regression tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    /// `CpuReadbackPlaneRepr` — `u64×2 + u32×4` = 32 bytes, align 8.
    #[test]
    fn cpu_readback_plane_repr_layout() {
        assert_eq!(offset_of!(CpuReadbackPlaneRepr, mapped_ptr), 0);
        assert_eq!(offset_of!(CpuReadbackPlaneRepr, byte_size), 8);
        assert_eq!(offset_of!(CpuReadbackPlaneRepr, width), 16);
        assert_eq!(offset_of!(CpuReadbackPlaneRepr, height), 20);
        assert_eq!(offset_of!(CpuReadbackPlaneRepr, bytes_per_pixel), 24);
        assert_eq!(offset_of!(CpuReadbackPlaneRepr, _padding), 28);
        assert_eq!(size_of::<CpuReadbackPlaneRepr>(), 32);
        assert_eq!(align_of::<CpuReadbackPlaneRepr>(), 8);
    }

    /// `CpuReadbackViewRepr` — `u32×4 + [CpuReadbackPlaneRepr; 4] @
    /// 16` = `16 + 128` = 144 bytes, align 8.
    #[test]
    fn cpu_readback_view_repr_layout() {
        assert_eq!(offset_of!(CpuReadbackViewRepr, format_raw), 0);
        assert_eq!(offset_of!(CpuReadbackViewRepr, width), 4);
        assert_eq!(offset_of!(CpuReadbackViewRepr, height), 8);
        assert_eq!(offset_of!(CpuReadbackViewRepr, plane_count), 12);
        assert_eq!(offset_of!(CpuReadbackViewRepr, planes), 16);
        assert_eq!(size_of::<CpuReadbackViewRepr>(), 144);
        assert_eq!(align_of::<CpuReadbackViewRepr>(), 8);
    }

    /// `HostSurfaceRegistrationRepr` — packed manifest layout.
    /// `u32×4 + i32 + u32 + u64×7` =
    /// `4+4+4+4 + 4+4 + 8+8+8 + 32` = 80 bytes, align 8. Dual-timeline
    /// (`produce_done` + `consume_done`) per
    /// `docs/architecture/adapter-timeline-single-writer.md`.
    #[test]
    fn host_surface_registration_repr_layout() {
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, format_raw), 0);
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, width), 4);
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, height), 8);
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, plane_count), 12);
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, initial_layout_raw), 16);
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, _padding), 20);
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, texture_handle), 24);
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, produce_done_handle), 32);
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, consume_done_handle), 40);
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, staging_handles), 48);
        assert_eq!(size_of::<HostSurfaceRegistrationRepr>(), 80);
        assert_eq!(align_of::<HostSurfaceRegistrationRepr>(), 8);
    }

    /// MAX_PLANES is locked at 4 — future formats with more planes
    /// require a layout-version bump because the array size
    /// changes and the surrounding offsets shift.
    #[test]
    fn max_planes_constant() {
        assert_eq!(MAX_PLANES, 4);
    }

    /// Locks the vtable's binary layout. Anchors every method slot
    /// at a fixed byte offset so a host built against vtable v1
    /// and a cdylib built against vtable v1 dispatch through the
    /// same offsets regardless of rustc-minor / dep-graph drift.
    /// New methods must append after `run_bridge_copy_buffer_to_image`
    /// and bump
    /// [`CPU_READBACK_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
    #[test]
    fn cpu_readback_surface_adapter_vtable_layout() {
        // layout_version: u32 @ 0
        // _reserved_padding: u32 @ 4
        // 13 fn pointers (8 bytes each) @ 8..112
        // total: 4 + 4 + 13*8 = 112 bytes, align 8
        assert_eq!(CPU_READBACK_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(size_of::<CpuReadbackSurfaceAdapterVTable>(), 112);
        assert_eq!(align_of::<CpuReadbackSurfaceAdapterVTable>(), 8);
        assert_eq!(offset_of!(CpuReadbackSurfaceAdapterVTable, layout_version), 0);
        assert_eq!(offset_of!(CpuReadbackSurfaceAdapterVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(CpuReadbackSurfaceAdapterVTable, clone_handle), 8);
        assert_eq!(offset_of!(CpuReadbackSurfaceAdapterVTable, drop_handle), 16);
        assert_eq!(
            offset_of!(CpuReadbackSurfaceAdapterVTable, register_host_surface),
            24
        );
        assert_eq!(
            offset_of!(CpuReadbackSurfaceAdapterVTable, unregister_host_surface),
            32
        );
        assert_eq!(
            offset_of!(CpuReadbackSurfaceAdapterVTable, registered_count),
            40
        );
        assert_eq!(offset_of!(CpuReadbackSurfaceAdapterVTable, acquire_read), 48);
        assert_eq!(offset_of!(CpuReadbackSurfaceAdapterVTable, acquire_write), 56);
        assert_eq!(
            offset_of!(CpuReadbackSurfaceAdapterVTable, try_acquire_read),
            64
        );
        assert_eq!(
            offset_of!(CpuReadbackSurfaceAdapterVTable, try_acquire_write),
            72
        );
        assert_eq!(
            offset_of!(CpuReadbackSurfaceAdapterVTable, end_read_access),
            80
        );
        assert_eq!(
            offset_of!(CpuReadbackSurfaceAdapterVTable, end_write_access),
            88
        );
        assert_eq!(
            offset_of!(CpuReadbackSurfaceAdapterVTable, run_bridge_copy_image_to_buffer),
            96
        );
        assert_eq!(
            offset_of!(CpuReadbackSurfaceAdapterVTable, run_bridge_copy_buffer_to_image),
            104
        );
    }

    /// Compile-time witness that the vtable is `Send + Sync`. A
    /// future regression that adds an interior-mutable or
    /// non-thread-safe field would break the unsafe impl above and
    /// trip this test at build time.
    #[test]
    fn vtable_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CpuReadbackSurfaceAdapterVTable>();
    }

    /// Zeroed view constructor wipes all plane slots.
    #[test]
    fn cpu_readback_view_repr_zeroed_clears_planes() {
        let v = CpuReadbackViewRepr::zeroed();
        assert_eq!(v.format_raw, 0);
        assert_eq!(v.plane_count, 0);
        for p in &v.planes {
            assert_eq!(p.mapped_ptr, 0);
            assert_eq!(p.byte_size, 0);
        }
    }

    /// Zeroed registration constructor wipes all staging slots.
    #[test]
    fn host_surface_registration_repr_zeroed_clears_handles() {
        let r = HostSurfaceRegistrationRepr::zeroed();
        assert_eq!(r.texture_handle, 0);
        assert_eq!(r.produce_done_handle, 0);
        assert_eq!(r.consume_done_handle, 0);
        for h in &r.staging_handles {
            assert_eq!(*h, 0);
        }
    }
}
