// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Plugin ABI callback table for `streamlib-adapter-opengl`.
//!
//! Sibling of [`streamlib-plugin-abi`]'s
//! `GpuContextLimitedAccessVTable` (issue #886) and
//! [`streamlib-adapter-vulkan-abi`]'s `VulkanSurfaceAdapterVTable`
//! (issue #887, the trunk piece this crate mirrors). Each adapter
//! ABI follows the same shape mechanically.
//!
//! # What this crate is
//!
//! A pure ABI contract describing how a host (`streamlib-engine`)
//! exposes its `OpenGlSurfaceAdapter` to a cdylib plugin without
//! sharing any Rust types beyond `#[repr(C)]` payloads and
//! `unsafe extern "C" fn` pointers. The cdylib carries a
//! `(handle, vtable)` PluginAbiObject; the host dispatches every method
//! through host-compiled code so layout drift between rustc-minor
//! versions and divergent dep graphs is contained inside the host
//! plugin.
//!
//! Dep posture mirrors [`streamlib-plugin-abi`]: zero streamlib
//! crates pulled, zero EGL/GL bindings, zero rustc-version-coupled
//! types. This keeps layout regression tests trivially runnable on
//! any host and the crate safe across the plugin ABI.
//!
//! # Audited cdylib-callable surface
//!
//! Every method on the cdylib-facing side of
//! `OpenGlSurfaceAdapter` / `OpenGlContext` / its acquire guards is
//! covered by exactly one slot below. The audit at pickup time
//! (against `libs/streamlib-adapter-opengl/src/`) enumerated:
//!
//! 1. `SurfaceAdapter` trait methods (from
//!    `streamlib-adapter-abi`) implemented on `OpenGlSurfaceAdapter`:
//!    `acquire_read`, `acquire_write`, `try_acquire_read`,
//!    `try_acquire_write`, `end_read_access`, `end_write_access`.
//! 2. Inherent methods on `OpenGlSurfaceAdapter`:
//!    `register_host_surface`, `register_external_oes_host_surface`,
//!    `unregister_host_surface`, `registered_count`.
//! 3. RAII guard `Drop` paths (`ReadGuard::drop` /
//!    `WriteGuard::drop`) route back through
//!    `SurfaceAdapter::end_*_access` — covered by the same vtable
//!    slots.
//! 4. View capability accessors (`OpenGlReadView::gl_texture_id`,
//!    `target`; `OpenGlWriteView::gl_texture_id`, `target`;
//!    `GlWritable::gl_texture_id`) are pure reads against the
//!    [`OpenGlViewRepr`] payload returned by `acquire_*`. No vtable
//!    hop on the view itself.
//!
//! `OpenGlContext` is a thin Clone-able convenience wrapper around
//! `Arc<OpenGlSurfaceAdapter>` — every method forwards to the same
//! adapter trait methods covered above. The vtable's handle is the
//! adapter Arc directly; the cdylib's `OpenGlContext` PluginAbiObject holds
//! the `(handle, vtable)` pair without an extra indirection.
//!
//! The `EglRuntime` accessor (`OpenGlSurfaceAdapter::runtime`) is
//! NOT cdylib-callable — every subprocess constructs its own
//! `EglRuntime` (the runtime owns thread-bound OpenGL contexts and
//! cannot meaningfully cross the plugin ABI even within one
//! process). No slot.

#![no_std]

use core::ffi::c_void;

// ============================================================================
// Layout version constants
// ============================================================================

/// Layout version of [`OpenGlSurfaceAdapterVTable`].
///
/// Pinned at offset 0 forever; new methods append to the end and
/// bump this constant. Host wiring asserts equality at install
/// time; cdylib code reads it before dereferencing any slot and
/// refuses to proceed on mismatch.
///
/// - v1: trunk lift — handle lifetime (`clone_handle` /
///   `drop_handle`) + 10 method slots covering the full
///   cdylib-callable surface (audit above). Locked by
///   [`tests::opengl_surface_adapter_vtable_layout`] and the
///   tier-1 null-handle tests in the source adapter crate.
pub const OPENGL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION: u32 = 1;

// ============================================================================
// View payload — pure data the host writes into a caller-provided slot
// ============================================================================

/// `#[repr(C)]` payload returned by `acquire_read` / `acquire_write` /
/// `try_acquire_*`.
///
/// Carries the live GL texture id and the binding target
/// (`GL_TEXTURE_2D` or `GL_TEXTURE_EXTERNAL_OES`). The cdylib's
/// `OpenGlReadView` / `OpenGlWriteView` PluginAbiObjects deref these
/// fields directly as POD reads (mirrors the cached-fields pattern
/// on `Texture` / `PixelBuffer` from `streamlib-plugin-abi` and
/// `VulkanViewRepr` from `streamlib-adapter-vulkan-abi`).
///
/// Lifetime: valid for the duration of the matching acquire scope
/// — the host's underlying `OpenGlReadView<'g>` /
/// `OpenGlWriteView<'g>` keeps the GL texture id alive via the
/// adapter's registry until `end_*_access` is called.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct OpenGlViewRepr {
    /// GL texture id (the `u32` returned by `glGenTextures`).
    pub gl_texture_id: u32,
    /// GL binding target enumerant — `GL_TEXTURE_2D` (`0x0DE1`) for
    /// host-allocated render-target-capable surfaces, or
    /// `GL_TEXTURE_EXTERNAL_OES` (`0x8D65`) for sampler-only
    /// surfaces registered via
    /// `register_external_oes_host_surface`.
    pub target: u32,
}

/// `#[repr(C)]` mirror of `streamlib_adapter_opengl::HostSurfaceRegistration`.
///
/// Layout MUST match the source struct byte-for-byte — the host
/// implementation populates this struct directly from the caller-
/// provided value and the cdylib's mirror reads the same offsets.
/// A cross-crate layout-equivalence test in
/// `streamlib-adapter-opengl::host_vtable` locks the source layout
/// (the abi crate is dep-free by design and cannot verify itself).
///
/// Adding fields requires a coordinated bump in both this crate AND
/// `streamlib-adapter-opengl::HostSurfaceRegistration`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HostSurfaceRegistrationRepr {
    /// DMA-BUF file descriptor exported from a host-allocated
    /// `VkImage`. The adapter dups it during EGL import; the
    /// caller may close their copy after registration returns.
    pub dma_buf_fd: i32,
    pub width: u32,
    pub height: u32,
    /// `DRM_FORMAT_*` four-character code for the surface's pixel
    /// layout.
    pub drm_fourcc: u32,
    /// DRM format modifier chosen by the host's allocator.
    pub drm_format_modifier: u64,
    pub plane_offset: u64,
    pub plane_stride: u64,
}

// ============================================================================
// OpenGlSurfaceAdapterVTable
// ============================================================================

/// Dispatch table for the host's `OpenGlSurfaceAdapter`.
///
/// The cdylib holds an opaque `*const c_void` handle (an
/// `Arc::into_raw(Arc<OpenGlSurfaceAdapter>)`-shaped pointer produced
/// by the host) plus a `*const OpenGlSurfaceAdapterVTable` it reads
/// from the `HostServices` payload when the cdylib PluginAbiObject lift
/// lands (sibling slice to this trunk PR). Method-dispatch callbacks
/// cover every cdylib-callable inherent method on
/// `OpenGlSurfaceAdapter` plus the `SurfaceAdapter` trait methods.
///
/// # Handle lifetime
///
/// `clone_handle` / `drop_handle` mirror the
/// `GpuContextLimitedAccessVTable` v2 pattern from
/// `streamlib-plugin-abi`: `clone_handle(borrowed) -> owned` bumps
/// the host's `Arc<OpenGlSurfaceAdapter>` refcount;
/// `drop_handle(owned)` releases. The owned handle remains valid
/// even after the originating runtime context is dropped, which
/// matches the existing `OpenGlContext: Clone` contract — a plugin
/// can stash a clone in `setup()` and hand it to a worker thread
/// that outlives the lifecycle call.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. New methods
/// append to the end and bump
/// [`OPENGL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
///
/// # Error crossing
///
/// Every fallible slot returns `i32` (0 = success, non-zero =
/// error). On error the host writes a UTF-8 message into the
/// caller-provided `err_buf` (clamped to `err_buf_cap`) and sets
/// `*err_len` to the bytes written. The wire shape mirrors the
/// host-side error-buffer convention already established by
/// `GpuContextLimitedAccessVTable::acquire_texture` and
/// `VulkanSurfaceAdapterVTable::acquire_read`; truncation never
/// trips a panic.
///
/// Non-error sentinels: `try_acquire_*` distinguishes "contended,
/// retry later" (status = 0, `*out_acquired = 0`) from "real
/// failure" (status = 1, error written) so the host's `Ok(None)`
/// vs `Err(...)` distinction survives the i32 crossing.
#[repr(C)]
pub struct OpenGlSurfaceAdapterVTable {
    /// Vtable layout version. Must equal
    /// [`OPENGL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    // -----------------------------------------------------------------
    // Handle lifetime
    // -----------------------------------------------------------------

    /// Take a borrowed handle (typically minted by the host's
    /// runtime context when wiring the cdylib-side `OpenGlContext`
    /// PluginAbiObject) and return a new owned handle with an Arc refcount
    /// bump on the underlying `Arc<OpenGlSurfaceAdapter>`. The
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
    // Registry management (inherent on OpenGlSurfaceAdapter)
    // -----------------------------------------------------------------

    /// Register a surface as a render-target-capable
    /// `GL_TEXTURE_2D`.
    ///
    /// `registration` carries the DMA-BUF fd, dimensions, fourcc,
    /// DRM modifier, plane offset, and plane stride. The host
    /// dups the fd during EGL import; the caller may close their
    /// copy after this returns.
    ///
    /// Returns 0 on success, non-zero on failure (e.g. surface_id
    /// already registered, EGL import failed, modifier
    /// `external_only=TRUE` so the resulting `EGLImage` cannot be
    /// bound as a `GL_TEXTURE_2D`). On error writes a UTF-8
    /// message into `err_buf`.
    pub register_host_surface: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id: u64,
        registration: *const HostSurfaceRegistrationRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Register a surface for sampler-only consumption via
    /// `GL_TEXTURE_EXTERNAL_OES`.
    ///
    /// Same DMA-BUF import path as
    /// [`Self::register_host_surface`], but binds the resulting
    /// `EGLImage` via
    /// `glEGLImageTargetTexture2DOES(GL_TEXTURE_EXTERNAL_OES, ...)`.
    /// Use this for surfaces the host did not (or could not)
    /// allocate with a render-target-capable modifier — typically
    /// camera ring textures whose underlying modifier is reported
    /// `external_only=TRUE` by `eglQueryDmaBufModifiersEXT` on
    /// NVIDIA.
    ///
    /// Returns 0 on success, non-zero on failure with a UTF-8
    /// message in `err_buf`.
    pub register_external_oes_host_surface: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id: u64,
        registration: *const HostSurfaceRegistrationRepr,
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
    /// success writes the populated [`OpenGlViewRepr`] into
    /// `*out_view` and returns 0. On contention the host returns 1
    /// with a "writer contended" message; the blocking variant
    /// never returns the contended-but-not-error case (`try_acquire_*`
    /// does).
    pub acquire_read: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut OpenGlViewRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Blocking write acquire. Same shape as
    /// [`Self::acquire_read`] but exclusive-write semantics; only
    /// applies to render-target-capable (`GL_TEXTURE_2D`) surfaces
    /// — `GL_TEXTURE_EXTERNAL_OES` surfaces return an error.
    pub acquire_write: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut OpenGlViewRepr,
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
        out_view: *mut OpenGlViewRepr,
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
        out_view: *mut OpenGlViewRepr,
        out_acquired: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Sealed: signal the release-side state for a read. Called by
    /// the cdylib's `ReadGuard::drop`. Idempotent against unknown
    /// surface IDs (the host logs and returns; no error surfaced
    /// because Drop can't propagate).
    pub end_read_access: unsafe extern "C" fn(handle: *const c_void, surface_id: u64),

    /// Sealed: signal the release-side state for a write. Called
    /// by the cdylib's `WriteGuard::drop`. The host issues
    /// `glFinish` inside this callback to flush the GL command
    /// stream so subsequent host Vulkan work sees the writes
    /// through the DMA-BUF.
    pub end_write_access: unsafe extern "C" fn(handle: *const c_void, surface_id: u64),
}

// Safety: every field is a primitive integer or an `unsafe extern "C" fn`
// pointer; no thread-local state, no interior mutability. The host
// guarantees the pointed-at adapter state outlives every cdylib that
// holds a clone of the handle via the loader's pinning shape.
unsafe impl Send for OpenGlSurfaceAdapterVTable {}
unsafe impl Sync for OpenGlSurfaceAdapterVTable {}

// ============================================================================
// Layout regression tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    /// `OpenGlViewRepr` carries (`gl_texture_id: u32`, `target: u32`).
    /// 8 bytes, align 4 — the smallest plugin ABI view payload in the
    /// adapter family.
    #[test]
    fn opengl_view_repr_layout() {
        assert_eq!(offset_of!(OpenGlViewRepr, gl_texture_id), 0);
        assert_eq!(offset_of!(OpenGlViewRepr, target), 4);
        assert_eq!(size_of::<OpenGlViewRepr>(), 8);
        assert_eq!(align_of::<OpenGlViewRepr>(), 4);
    }

    /// `HostSurfaceRegistrationRepr` MUST mirror
    /// `streamlib_adapter_opengl::HostSurfaceRegistration`
    /// byte-for-byte. The source layout is locked by a cross-crate
    /// test in the adapter crate (this abi crate is dep-free).
    ///
    /// Field order: dma_buf_fd: i32 @ 0, width: u32 @ 4,
    /// height: u32 @ 8, drm_fourcc: u32 @ 12, drm_format_modifier:
    /// u64 @ 16, plane_offset: u64 @ 24, plane_stride: u64 @ 32.
    /// Total: 40 bytes, align 8.
    #[test]
    fn host_surface_registration_repr_layout() {
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, dma_buf_fd), 0);
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, width), 4);
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, height), 8);
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, drm_fourcc), 12);
        assert_eq!(
            offset_of!(HostSurfaceRegistrationRepr, drm_format_modifier),
            16
        );
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, plane_offset), 24);
        assert_eq!(offset_of!(HostSurfaceRegistrationRepr, plane_stride), 32);
        assert_eq!(size_of::<HostSurfaceRegistrationRepr>(), 40);
        assert_eq!(align_of::<HostSurfaceRegistrationRepr>(), 8);
    }

    /// Locks the vtable's binary layout. Anchors every method slot
    /// at a fixed byte offset so a host built against vtable v1
    /// and a cdylib built against vtable v1 dispatch through the
    /// same offsets regardless of rustc-minor / dep-graph drift.
    /// New methods must append after `end_write_access` and bump
    /// [`OPENGL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
    #[test]
    fn opengl_surface_adapter_vtable_layout() {
        // layout_version: u32 @ 0
        // _reserved_padding: u32 @ 4
        // 12 fn pointers (8 bytes each) @ 8..104
        // total: 4 + 4 + 12*8 = 104 bytes, align 8
        assert_eq!(OPENGL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(size_of::<OpenGlSurfaceAdapterVTable>(), 104);
        assert_eq!(align_of::<OpenGlSurfaceAdapterVTable>(), 8);
        assert_eq!(offset_of!(OpenGlSurfaceAdapterVTable, layout_version), 0);
        assert_eq!(offset_of!(OpenGlSurfaceAdapterVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(OpenGlSurfaceAdapterVTable, clone_handle), 8);
        assert_eq!(offset_of!(OpenGlSurfaceAdapterVTable, drop_handle), 16);
        assert_eq!(
            offset_of!(OpenGlSurfaceAdapterVTable, register_host_surface),
            24
        );
        assert_eq!(
            offset_of!(OpenGlSurfaceAdapterVTable, register_external_oes_host_surface),
            32
        );
        assert_eq!(
            offset_of!(OpenGlSurfaceAdapterVTable, unregister_host_surface),
            40
        );
        assert_eq!(
            offset_of!(OpenGlSurfaceAdapterVTable, registered_count),
            48
        );
        assert_eq!(offset_of!(OpenGlSurfaceAdapterVTable, acquire_read), 56);
        assert_eq!(offset_of!(OpenGlSurfaceAdapterVTable, acquire_write), 64);
        assert_eq!(
            offset_of!(OpenGlSurfaceAdapterVTable, try_acquire_read),
            72
        );
        assert_eq!(
            offset_of!(OpenGlSurfaceAdapterVTable, try_acquire_write),
            80
        );
        assert_eq!(
            offset_of!(OpenGlSurfaceAdapterVTable, end_read_access),
            88
        );
        assert_eq!(
            offset_of!(OpenGlSurfaceAdapterVTable, end_write_access),
            96
        );
    }

    /// Compile-time witness that the vtable is `Send + Sync`. A
    /// future regression that adds an interior-mutable or non-thread-
    /// safe field would break the unsafe impl above and trip this
    /// test at build time.
    #[test]
    fn vtable_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OpenGlSurfaceAdapterVTable>();
    }
}
