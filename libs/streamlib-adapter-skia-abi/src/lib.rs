// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Plugin ABI callback table for `streamlib-adapter-skia`.
//!
//! Sibling of [`streamlib-adapter-vulkan-abi`] and
//! [`streamlib-plugin-abi`] in the per-adapter vtable lift family
//! (`GpuContextLimitedAccessVTable` and friends from issue #886; the
//! per-adapter trunk in issue #887 / PR #994 established the
//! mechanical shape this crate follows).
//!
//! # What this crate is
//!
//! Pure ABI contract describing how a host (`streamlib-engine`)
//! exposes its Skia surface adapters — `SkiaSurfaceAdapter<D>`
//! (Vulkan-backed, generic over device flavor) and
//! `SkiaGlSurfaceAdapter` (OpenGL-backed, non-generic) — to a cdylib
//! plugin without sharing any Rust types beyond `#[repr(C)]` payloads
//! and `unsafe extern "C" fn` pointers.
//!
//! Dep posture mirrors [`streamlib-plugin-abi`]: zero streamlib
//! crates pulled, zero `skia-safe`, zero vulkanalia, zero
//! rustc-version-coupled types. Skia bindings (`skia-safe`) are
//! deliberately excluded — see "Audit findings — what does NOT cross
//! the vtable" below.
//!
//! # Audited cdylib-callable surface
//!
//! The audit at pickup time (against
//! `libs/streamlib-adapter-skia/src/`) enumerated:
//!
//! 1. `SurfaceAdapter` trait methods (from `streamlib-adapter-abi`)
//!    implemented on both Skia adapter flavors: `acquire_read`,
//!    `acquire_write`, `try_acquire_read`, `try_acquire_write`,
//!    `end_read_access`, `end_write_access`.
//! 2. RAII guard `Drop` paths (`ReadGuard::drop` / `WriteGuard::drop`)
//!    route back through `SurfaceAdapter::end_*_access` — covered
//!    by the same vtable slots.
//!
//! # Audit findings — what does NOT cross the vtable
//!
//! Skia is **host-side only** by deliberate architectural choice (per
//! [`docs/architecture/subprocess-rhi-parity.md`]). Cdylibs do NOT
//! depend on `streamlib-adapter-skia` in their Cargo dep graph;
//! subprocess customers reach Skia surfaces through the wrapped
//! Vulkan or OpenGL adapter's cdylib path. Consequently:
//!
//! - **No inherent registry methods** to expose. Unlike
//!   `VulkanSurfaceAdapter` (which carries `register_host_surface`,
//!   `unregister_host_surface`, `release_to_foreign`,
//!   `surface_image_info`, `raw_handles`, `registered_count`), the
//!   Skia adapters carry zero such inherent methods — registry
//!   operations route through the inner Vulkan/OpenGL adapter via
//!   the host-side `SkiaSurfaceAdapter::inner()` accessor (which is
//!   itself not cdylib-callable since it returns
//!   `&Arc<VulkanSurfaceAdapter<D>>`). Only `registered_count` has
//!   a useful informational projection through the inner adapter;
//!   it's included here for symmetry with the Vulkan/OpenGL ABI.
//!
//! - **No Skia-typed view accessors.** `SkiaWriteView` exposes
//!   `&skia_safe::Surface`; `SkiaReadView` exposes
//!   `&skia_safe::Image`. Both Skia types are `RCHandle<…>` raw
//!   pointer wrappers tied to `skia-safe`'s `!Send + !Sync`
//!   single-thread-affine `GrDirectContext`. Their internal layout
//!   is **rustc-version-coupled and skia-safe-version-coupled** —
//!   crossing them through an extern "C" boundary would defeat the
//!   purpose of this ABI. Adding Skia view access across the plugin ABI is
//!   future work (the Option C msgpack display-list shape recorded
//!   on issue #889's body is a candidate; a per-method canvas
//!   vtable is another).
//!
//! - **No `inner()` accessor.** The Vulkan / OpenGL inner adapters
//!   are reachable through their own vtables — a cdylib that needs
//!   raw GPU handles uses the Vulkan or OpenGL adapter's vtable
//!   directly, not Skia's.
//!
//! # What this vtable does cover
//!
//! The scoping contract — acquire / release on a `StreamlibSurface`
//! — is identical across every surface adapter and IS safe across
//! the plugin ABI via the `surface_id`-only [`SkiaViewRepr`] payload. The
//! vtable carries the trunk-pattern `clone_handle` /
//! `drop_handle` lifetime slots plus all 6 `SurfaceAdapter` trait
//! methods plus the informational `registered_count` projection.
//!
//! This is the minimum useful surface for a future cdylib Skia
//! consumer that wants to pair its own host-side or
//! escalate-IPC-mediated Skia draw flow with the host's surface
//! lifecycle. The lifecycle bit is what the trunk pattern locks at
//! ABI birth so the five sibling adapters share the same scoping
//! shape.

#![no_std]

use core::ffi::c_void;

// ============================================================================
// Layout version constants
// ============================================================================

/// Layout version of [`SkiaSurfaceAdapterVTable`].
///
/// Pinned at offset 0 forever; new methods append to the end and
/// bump this constant. Host wiring asserts equality at install
/// time; cdylib code reads it before dereferencing any slot and
/// refuses to proceed on mismatch.
///
/// - v1: trunk lift — handle lifetime (`clone_handle` /
///   `drop_handle`) + `registered_count` + 6 `SurfaceAdapter` trait
///   slots. Locked by
///   [`tests::skia_surface_adapter_vtable_layout`] and the
///   tier-1 null-handle tests next to the host wiring.
pub const SKIA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Layout version of [`SkiaGlSurfaceAdapterVTable`]. Same shape as
/// [`SKIA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`] but distinct so
/// the two vtables version independently as their underlying
/// adapters evolve.
pub const SKIA_GL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION: u32 = 1;

// ============================================================================
// View payload — pure data the host writes into a caller-provided slot
// ============================================================================

/// `#[repr(C)]` payload returned by `acquire_read` / `acquire_write` /
/// `try_acquire_*`.
///
/// Deliberately **minimal** compared to
/// [`streamlib_adapter_vulkan_abi::VulkanViewRepr`]: Skia's view
/// types (`skia_safe::Surface` / `skia_safe::Image`) are
/// `RCHandle<…>` wrappers tied to the host's `skia-safe` version
/// and `GrDirectContext` — they cannot be safely projected through
/// an extern "C" boundary, so the only meaningful per-acquire data
/// that travels here is the surface identity itself plus its
/// dimensions (useful for the cdylib to gate scope-local
/// pre/post bookkeeping without re-reading the
/// `StreamlibSurface`).
///
/// Future expansion (Option C msgpack display-list submit, per-call
/// canvas vtable) appends fields here and bumps the layout version;
/// see this crate's module docs for the recorded alternatives.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SkiaViewRepr {
    /// Surface identity the host's adapter associated with this
    /// scope. Mirrors `WriteGuard::surface_id` / `ReadGuard::surface_id`
    /// on the host side.
    pub surface_id: u64,

    /// Surface width in pixels. Copied from
    /// `StreamlibSurface::width` for convenience.
    pub width: u32,

    /// Surface height in pixels. Copied from
    /// `StreamlibSurface::height` for convenience.
    pub height: u32,

    /// Reserved bytes for additive ABI extensions. MUST be zeroed.
    /// Sized so the struct rounds out to 32 bytes and any future
    /// `u64` / pointer additions land naturally aligned.
    pub _reserved: [u8; 16],
}

// ============================================================================
// SkiaSurfaceAdapterVTable — Vulkan-backed Skia adapter, generic over D
// ============================================================================

/// Dispatch table for the host's `SkiaSurfaceAdapter<D>` (the
/// Vulkan-backed Skia adapter).
///
/// The cdylib holds an opaque `*const c_void` handle (an
/// `Arc::into_raw(Arc<SkiaSurfaceAdapter<D>>)`-shaped pointer
/// produced by the host) plus a
/// `*const SkiaSurfaceAdapterVTable` it reads from the
/// `HostServices` payload when the cdylib PluginAbiObject lift lands
/// (sibling slice to this trunk PR). Method-dispatch callbacks
/// cover every cdylib-callable `SurfaceAdapter` trait method.
///
/// # Handle lifetime
///
/// `clone_handle` / `drop_handle` mirror the
/// `GpuContextLimitedAccessVTable` v2 pattern from
/// `streamlib-plugin-abi`: `clone_handle(borrowed) -> owned` bumps
/// the host's `Arc<SkiaSurfaceAdapter<D>>` refcount;
/// `drop_handle(owned)` releases. The owned handle remains valid
/// even after the originating runtime context is dropped.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. New methods
/// append to the end and bump
/// [`SKIA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
///
/// # Error crossing
///
/// Every fallible slot returns `i32` (0 = success, non-zero =
/// error). On error the host writes a UTF-8 message into the
/// caller-provided `err_buf` (clamped to `err_buf_cap`) and sets
/// `*err_len` to the bytes written. Identical to
/// `streamlib_adapter_vulkan_abi::VulkanSurfaceAdapterVTable`'s
/// error convention.
///
/// Non-error sentinels: `try_acquire_*` distinguishes "contended,
/// retry later" (status = 0, `*out_acquired = 0`) from "real
/// failure" (status = 1, error written) so the host's `Ok(None)`
/// vs `Err(...)` distinction survives the i32 crossing.
#[repr(C)]
pub struct SkiaSurfaceAdapterVTable {
    /// Vtable layout version. Must equal
    /// [`SKIA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    // -----------------------------------------------------------------
    // Handle lifetime
    // -----------------------------------------------------------------
    /// Take a borrowed handle and return a new owned handle with an
    /// Arc refcount bump on the underlying
    /// `Arc<SkiaSurfaceAdapter<D>>`. The owned handle remains valid
    /// even after the originating context is dropped and MUST be
    /// released exactly once via [`Self::drop_handle`]. Calling on
    /// a null pointer returns null.
    pub clone_handle: unsafe extern "C" fn(borrowed_handle: *const c_void) -> *const c_void,

    /// Release an owned handle previously obtained from
    /// [`Self::clone_handle`]. Calling on a null pointer is a
    /// no-op.
    pub drop_handle: unsafe extern "C" fn(owned_handle: *const c_void),

    // -----------------------------------------------------------------
    // Informational projection
    // -----------------------------------------------------------------
    /// Snapshot the inner Vulkan adapter's registry size (number of
    /// currently-registered surfaces). Returns 0 on null handle.
    /// The Skia adapter has no registry of its own — this projects
    /// through `SkiaSurfaceAdapter::inner().registered_count()`.
    pub registered_count: unsafe extern "C" fn(handle: *const c_void) -> usize,

    // -----------------------------------------------------------------
    // SurfaceAdapter trait methods
    // -----------------------------------------------------------------
    /// Blocking read acquire.
    ///
    /// `surface_ptr` is a `*const StreamlibSurface` borrowed from
    /// the caller's stack; valid for the duration of the call. On
    /// success writes the populated [`SkiaViewRepr`] into
    /// `*out_view` and returns 0. The blocking variant never
    /// returns the contended-but-not-error case (`try_acquire_*`
    /// does).
    pub acquire_read: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut SkiaViewRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Blocking write acquire. Same shape as [`Self::acquire_read`]
    /// but exclusive-write semantics.
    pub acquire_write: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut SkiaViewRepr,
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
        out_view: *mut SkiaViewRepr,
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
        out_view: *mut SkiaViewRepr,
        out_acquired: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Sealed: notify the Skia adapter that a read access scope has
    /// ended. Called by the cdylib's `ReadGuard::drop`. Idempotent
    /// against unknown surface IDs (the host logs and returns; no
    /// error surfaced because Drop can't propagate).
    ///
    /// Note: `SkiaSurfaceAdapter::end_read_access` is a host-side
    /// no-op by deliberate design (Skia's view drop hook is where
    /// flush + inner-guard-drop happens), so this slot exists for
    /// trunk-pattern symmetry and host-side observability rather
    /// than functional release semantics.
    pub end_read_access: unsafe extern "C" fn(handle: *const c_void, surface_id: u64),

    /// Sealed: notify the Skia adapter that a write access scope
    /// has ended. Called by the cdylib's `WriteGuard::drop`. Same
    /// note as [`Self::end_read_access`] — host-side no-op today,
    /// kept for trunk-pattern symmetry.
    pub end_write_access: unsafe extern "C" fn(handle: *const c_void, surface_id: u64),
}

// Safety: every field is a primitive integer or an `unsafe extern "C" fn`
// pointer; no thread-local state, no interior mutability. The host
// guarantees the pointed-at adapter state outlives every cdylib that
// holds a clone of the handle via the loader's pinning shape.
unsafe impl Send for SkiaSurfaceAdapterVTable {}
unsafe impl Sync for SkiaSurfaceAdapterVTable {}

// ============================================================================
// SkiaGlSurfaceAdapterVTable — OpenGL-backed Skia adapter, non-generic
// ============================================================================

/// Dispatch table for the host's `SkiaGlSurfaceAdapter` (the
/// OpenGL-backed Skia adapter).
///
/// Identical method surface to [`SkiaSurfaceAdapterVTable`] —
/// `SkiaGlSurfaceAdapter` implements the same `SurfaceAdapter`
/// trait — but the type is separate because the underlying adapter
/// is non-generic (composes on `Arc<OpenGlSurfaceAdapter>`, which
/// has no device-flavor parameterization). Versioning the two
/// vtables independently keeps additive ABI extensions to the
/// Vulkan-backed flavor (e.g. eventually exposing the inner
/// adapter's Vulkan handles) from forcing a no-op bump on the GL
/// flavor.
///
/// Layout discipline + error crossing are identical to
/// [`SkiaSurfaceAdapterVTable`].
#[repr(C)]
pub struct SkiaGlSurfaceAdapterVTable {
    /// Vtable layout version. Must equal
    /// [`SKIA_GL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding.
    pub _reserved_padding: u32,

    /// Take a borrowed handle and return a new owned handle.
    /// Mirrors [`SkiaSurfaceAdapterVTable::clone_handle`].
    pub clone_handle: unsafe extern "C" fn(borrowed_handle: *const c_void) -> *const c_void,

    /// Release an owned handle. Mirrors
    /// [`SkiaSurfaceAdapterVTable::drop_handle`].
    pub drop_handle: unsafe extern "C" fn(owned_handle: *const c_void),

    /// Snapshot the inner OpenGL adapter's registry size. Projects
    /// through `SkiaGlSurfaceAdapter::inner().registered_count()`.
    pub registered_count: unsafe extern "C" fn(handle: *const c_void) -> usize,

    /// Blocking read acquire. See
    /// [`SkiaSurfaceAdapterVTable::acquire_read`].
    pub acquire_read: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut SkiaViewRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Blocking write acquire. See
    /// [`SkiaSurfaceAdapterVTable::acquire_write`].
    pub acquire_write: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut SkiaViewRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Non-blocking read acquire. See
    /// [`SkiaSurfaceAdapterVTable::try_acquire_read`].
    pub try_acquire_read: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut SkiaViewRepr,
        out_acquired: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Non-blocking write acquire. See
    /// [`SkiaSurfaceAdapterVTable::try_acquire_write`].
    pub try_acquire_write: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut SkiaViewRepr,
        out_acquired: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Sealed: end-of-read-scope hook. See
    /// [`SkiaSurfaceAdapterVTable::end_read_access`].
    pub end_read_access: unsafe extern "C" fn(handle: *const c_void, surface_id: u64),

    /// Sealed: end-of-write-scope hook. See
    /// [`SkiaSurfaceAdapterVTable::end_write_access`].
    pub end_write_access: unsafe extern "C" fn(handle: *const c_void, surface_id: u64),
}

unsafe impl Send for SkiaGlSurfaceAdapterVTable {}
unsafe impl Sync for SkiaGlSurfaceAdapterVTable {}

// ============================================================================
// Layout regression tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    /// `SkiaViewRepr` is `(u64, u32, u32, [u8; 16])` = 32 bytes,
    /// align 8. Locks the byte offsets so a host built against
    /// v1 and a cdylib built against v1 read the same fields at
    /// the same offsets regardless of rustc-minor / dep-graph
    /// drift. New fields append after `_reserved` (which then
    /// shrinks accordingly) and the layout version bumps.
    #[test]
    fn skia_view_repr_layout() {
        assert_eq!(offset_of!(SkiaViewRepr, surface_id), 0);
        assert_eq!(offset_of!(SkiaViewRepr, width), 8);
        assert_eq!(offset_of!(SkiaViewRepr, height), 12);
        assert_eq!(offset_of!(SkiaViewRepr, _reserved), 16);
        assert_eq!(size_of::<SkiaViewRepr>(), 32);
        assert_eq!(align_of::<SkiaViewRepr>(), 8);
    }

    /// Locks the Vulkan-backed Skia vtable binary layout. Anchors
    /// every method slot at a fixed byte offset.
    ///
    /// 9 method slots: 2 lifetime + 1 informational + 6
    /// SurfaceAdapter trait. Header is `u32 layout_version` +
    /// `u32 _reserved_padding` = 8 bytes. Total = 8 + 9*8 = 80
    /// bytes, align 8.
    #[test]
    fn skia_surface_adapter_vtable_layout() {
        assert_eq!(SKIA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(size_of::<SkiaSurfaceAdapterVTable>(), 80);
        assert_eq!(align_of::<SkiaSurfaceAdapterVTable>(), 8);
        assert_eq!(offset_of!(SkiaSurfaceAdapterVTable, layout_version), 0);
        assert_eq!(offset_of!(SkiaSurfaceAdapterVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(SkiaSurfaceAdapterVTable, clone_handle), 8);
        assert_eq!(offset_of!(SkiaSurfaceAdapterVTable, drop_handle), 16);
        assert_eq!(offset_of!(SkiaSurfaceAdapterVTable, registered_count), 24);
        assert_eq!(offset_of!(SkiaSurfaceAdapterVTable, acquire_read), 32);
        assert_eq!(offset_of!(SkiaSurfaceAdapterVTable, acquire_write), 40);
        assert_eq!(offset_of!(SkiaSurfaceAdapterVTable, try_acquire_read), 48);
        assert_eq!(offset_of!(SkiaSurfaceAdapterVTable, try_acquire_write), 56);
        assert_eq!(offset_of!(SkiaSurfaceAdapterVTable, end_read_access), 64);
        assert_eq!(offset_of!(SkiaSurfaceAdapterVTable, end_write_access), 72);
    }

    /// Locks the OpenGL-backed Skia vtable binary layout. Same
    /// shape and size as [`SkiaSurfaceAdapterVTable`] — the type
    /// is separate so the two vtables can version independently as
    /// their underlying adapters evolve.
    #[test]
    fn skia_gl_surface_adapter_vtable_layout() {
        assert_eq!(SKIA_GL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(size_of::<SkiaGlSurfaceAdapterVTable>(), 80);
        assert_eq!(align_of::<SkiaGlSurfaceAdapterVTable>(), 8);
        assert_eq!(offset_of!(SkiaGlSurfaceAdapterVTable, layout_version), 0);
        assert_eq!(offset_of!(SkiaGlSurfaceAdapterVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(SkiaGlSurfaceAdapterVTable, clone_handle), 8);
        assert_eq!(offset_of!(SkiaGlSurfaceAdapterVTable, drop_handle), 16);
        assert_eq!(offset_of!(SkiaGlSurfaceAdapterVTable, registered_count), 24);
        assert_eq!(offset_of!(SkiaGlSurfaceAdapterVTable, acquire_read), 32);
        assert_eq!(offset_of!(SkiaGlSurfaceAdapterVTable, acquire_write), 40);
        assert_eq!(offset_of!(SkiaGlSurfaceAdapterVTable, try_acquire_read), 48);
        assert_eq!(
            offset_of!(SkiaGlSurfaceAdapterVTable, try_acquire_write),
            56
        );
        assert_eq!(offset_of!(SkiaGlSurfaceAdapterVTable, end_read_access), 64);
        assert_eq!(offset_of!(SkiaGlSurfaceAdapterVTable, end_write_access), 72);
    }

    /// Compile-time witness that both vtables are `Send + Sync`.
    /// A future regression that adds an interior-mutable or non-
    /// thread-safe field would break the unsafe impls above and
    /// trip this test at build time.
    #[test]
    fn vtables_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SkiaSurfaceAdapterVTable>();
        assert_send_sync::<SkiaGlSurfaceAdapterVTable>();
    }
}
