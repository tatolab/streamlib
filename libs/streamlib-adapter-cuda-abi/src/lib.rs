// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-DSO callback table for `streamlib-adapter-cuda`.
//!
//! Sibling of `streamlib-adapter-vulkan-abi` (#887) following the same
//! shape mechanically. Per-adapter sibling of the umbrella callback-
//! table plugin ABI migration (#877 → #886 → #887/#888/#889/#890/#891).
//!
//! # What this crate is
//!
//! A pure ABI contract describing how a host (`streamlib-engine`)
//! exposes its `CudaSurfaceAdapter` to a cdylib plugin without sharing
//! any Rust types beyond `#[repr(C)]` payloads and
//! `unsafe extern "C" fn` pointers. The cdylib carries a
//! `(handle, vtable)` β-shape; the host dispatches every method
//! through host-compiled code so layout drift between rustc-minor
//! versions and divergent dep graphs is contained inside the host
//! DSO.
//!
//! Dep posture mirrors [`streamlib-plugin-abi`] and
//! `streamlib-adapter-vulkan-abi`: zero streamlib crates pulled, zero
//! vulkanalia, zero cudarc, zero dlpark, zero rustc-version-coupled
//! types. This keeps layout regression tests trivially runnable on
//! any host and the crate cross-DSO-safe.
//!
//! # Audited cdylib-callable surface
//!
//! The cuda adapter exposes TWO resource flavors on the same
//! [`CudaSurfaceAdapter<D>`]:
//!
//! - **Buffer flavor** (DLPack flat-tensor path): OPAQUE_FD
//!   `VkBuffer` that the cdylib re-imports into CUDA via
//!   `cudaImportExternalMemory(OPAQUE_FD)` +
//!   `cudaExternalMemoryGetMappedBuffer` → flat `void*`.
//!   `acquire_read` / `acquire_write` (and their `try_*` siblings)
//!   produce buffer-flavored views.
//! - **Image flavor** (tiled mipmapped-array path): OPAQUE_FD
//!   `VkImage` that the cdylib re-imports via
//!   `cudaImportExternalMemory(OPAQUE_FD)` +
//!   `cudaExternalMemoryGetMappedMipmappedArray` →
//!   `cudaTextureObject_t` / `cudaSurfaceObject_t`. Image acquires
//!   ride their own pair: `acquire_texture` (read-only sampling),
//!   `acquire_surface` (read-write surface ops), each with `try_*`
//!   siblings.
//!
//! Both flavors share the same release path — guard drop fires
//! `end_read_access` / `end_write_access` against the surface_id;
//! the resource discriminator is internal to the adapter's registry.
//!
//! Every method on the cdylib-facing side of `CudaSurfaceAdapter<D>`
//! / `CudaContext<D>` / its acquire guards is covered by exactly one
//! slot below. The audit (against `libs/streamlib-adapter-cuda/src/`)
//! enumerated:
//!
//! 1. `SurfaceAdapter` trait methods (from `streamlib-adapter-abi`)
//!    implemented on `CudaSurfaceAdapter` for the buffer flavor:
//!    `acquire_read`, `acquire_write`, `try_acquire_read`,
//!    `try_acquire_write`, `end_read_access`, `end_write_access`.
//! 2. Image-flavored inherent methods on `CudaSurfaceAdapter<D>`:
//!    `acquire_texture`, `try_acquire_texture`, `acquire_surface`,
//!    `try_acquire_surface`. Release for image guards is the same
//!    `end_read_access` / `end_write_access` pair as the buffer
//!    flavor (the registry's discriminator decides the resource
//!    type per surface_id).
//! 3. Registry inherent methods: `register_host_surface` (buffer),
//!    `register_host_image_surface` (image), `unregister_host_surface`,
//!    `registered_count`.
//! 4. RAII guard `Drop` paths (`ReadGuard::drop` /
//!    `WriteGuard::drop` for buffer; `CudaTextureGuard::drop` /
//!    `CudaSurfaceGuard::drop` for image) all route back through
//!    `end_*_access` — covered by the same vtable slots.
//! 5. View capability accessors (`CudaReadView::vk_buffer` / `size`;
//!    `CudaWriteView::vk_buffer` / `size`; `CudaTextureView::vk_image`
//!    / `width` / `height` / `format`; `CudaSurfaceView::vk_image` /
//!    `width` / `height` / `format`) are pure reads against the
//!    [`CudaBufferViewRepr`] / [`CudaImageViewRepr`] payloads
//!    returned by `acquire_*`. No vtable hop on the view itself.
//!
//! # What is explicitly NOT in the vtable
//!
//! - **DLPack capsule construction** (`CudaReadView::dlpack_managed_tensor`
//!   etc.). The `dlpark::ffi` mirrors are themselves `#[repr(C)]` per
//!   the DLPack v0.8 C spec — pinned to `=0.6.0` in the workspace
//!   lockfile and layout-locked by tests in
//!   `streamlib-adapter-cuda::dlpack::tests`. Cdylibs construct
//!   DLPack capsules entirely on their own side: pull `CUdeviceptr`
//!   via `cudaExternalMemoryGetMappedBuffer`, build a `ManagedTensor`
//!   using the same DLPack C ABI. The vtable's job is to surface the
//!   underlying `vk::Buffer` handle + size so the cdylib has what it
//!   needs to import into CUDA. No `DLTensor` mirror in this crate.
//!
//! - **Power-user `surface_pixel_buffer` / `surface_texture` /
//!   `surface_timeline` accessors**. These return `Arc<HostInternal>`
//!   shapes (used by in-process carve-out helpers to call
//!   `export_opaque_fd_memory()`) and would leak host-internal layouts
//!   across the cdylib boundary. Cdylib customers reach the same FDs
//!   through surface-share IPC at registration time; they don't need
//!   to re-export them.
//!
//! - **`submit_host_copy_image_to_buffer`**. Host-pipeline producer
//!   work that takes `&T: VulkanTextureLike + ?Sized` — a generic
//!   trait the cdylib can't pass an arbitrary `&T` through. The
//!   producer is host code; the cdylib is the downstream CUDA
//!   consumer of the buffer the producer writes into.
//!
//! - **Chaining builder `with_acquire_timeout`** — host-side
//!   construction-time configuration; the configured adapter is what
//!   crosses the vtable handle.
//!
//! - **Test-only `submit_pool_create_count`** — `#[doc(hidden)]` hook
//!   for in-tree tests that lock #620's amortisation invariant.

#![no_std]

use core::ffi::c_void;

// ============================================================================
// Layout version constants
// ============================================================================

/// Layout version of [`CudaSurfaceAdapterVTable`].
///
/// Pinned at offset 0 forever; new methods append to the end and
/// bump this constant. Host wiring asserts equality at install
/// time; cdylib code reads it before dereferencing any slot and
/// refuses to proceed on mismatch.
///
/// - v1: trunk lift — handle lifetime (`clone_handle` /
///   `drop_handle`) + 14 method slots covering the full
///   cdylib-callable surface (audit above). Locked by
///   [`tests::cuda_surface_adapter_vtable_layout`] and the tier-1
///   null-handle tests in
///   `streamlib-adapter-cuda::host_vtable::tier1_null_handle_tests`.
pub const CUDA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION: u32 = 1;

// ============================================================================
// View payloads — pure data the host writes into a caller-provided slot
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

/// `TextureFormat` enumerant value.
///
/// Mirrors `streamlib_consumer_rhi::TextureFormat`'s `#[repr(u32)]`
/// representation byte-for-byte. The cdylib reconstructs the
/// strongly-typed enum via `TextureFormat::from_repr(raw)` (or its
/// equivalent) once the value crosses the vtable.
///
/// The CUDA adapter restricts image-flavored surfaces to the
/// CUDA-mappable subset (`Rgba8Unorm = 0`, `Rgba16Float = 4`,
/// `Rgba32Float = 5`) at registration time; cdylibs that build a
/// `cudaTextureObject_t` from the imported mipmapped array can rely
/// on the value being one of those three.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TextureFormatRepr(pub u32);

/// `#[repr(C)]` payload returned by `acquire_read` / `acquire_write` /
/// `try_acquire_*` for the **buffer flavor** of the cuda adapter.
///
/// Carries the live `vk::Buffer` handle and its size in bytes —
/// everything the cdylib needs to call
/// `cudaImportExternalMemory(OPAQUE_FD)` +
/// `cudaExternalMemoryGetMappedBuffer` and construct a DLPack
/// `ManagedTensor` over the resulting flat device pointer. The
/// cdylib's `CudaReadView` / `CudaWriteView` β-shapes deref these
/// fields directly as POD reads.
///
/// Lifetime: valid for the duration of the matching acquire scope
/// — the host's underlying `CudaReadView<'g>` / `CudaWriteView<'g>`
/// keeps the `VkBuffer` alive via the adapter's registry until
/// `end_*_access` is called.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CudaBufferViewRepr {
    /// Opaque `VkBuffer` handle (Vulkan handles are 64-bit per spec).
    pub vk_buffer: u64,
    /// Buffer size in bytes (`VkDeviceSize` is `u64` per spec).
    pub size: u64,
}

/// `#[repr(C)]` payload returned by `acquire_texture` /
/// `acquire_surface` / `try_acquire_*` for the **image flavor** of
/// the cuda adapter.
///
/// Carries the live `vk::Image` handle plus dimensions + format —
/// everything the cdylib needs to call
/// `cudaImportExternalMemory(OPAQUE_FD)` +
/// `cudaExternalMemoryGetMappedMipmappedArray` and construct a
/// `cudaTextureObject_t` (read-only) or `cudaSurfaceObject_t`
/// (read-write). The cdylib's `CudaTextureView` / `CudaSurfaceView`
/// β-shapes deref these fields directly as POD reads.
///
/// `format` is the [`TextureFormatRepr`] enumerant; the adapter
/// guarantees it's in the CUDA-mappable subset (`Rgba8Unorm`,
/// `Rgba16Float`, `Rgba32Float`) by the registration-time check in
/// `CudaSurfaceAdapter::register_host_image_surface`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CudaImageViewRepr {
    /// Opaque `VkImage` handle (Vulkan handles are 64-bit per spec).
    pub vk_image: u64,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// `TextureFormat` enumerant — see [`TextureFormatRepr`].
    pub format: TextureFormatRepr,
    /// Padding so the struct's total size is a multiple of 8.
    /// Zero today, never read.
    pub _padding: u32,
}

// ============================================================================
// CudaSurfaceAdapterVTable
// ============================================================================

/// Dispatch table for the host's `CudaSurfaceAdapter<D>`.
///
/// The cdylib holds an opaque `*const c_void` handle (an
/// `Arc::into_raw(Arc<CudaSurfaceAdapter<D>>)`-shaped pointer
/// produced by the host) plus a `*const CudaSurfaceAdapterVTable`
/// it reads from the `HostServices` payload when the cdylib β-shape
/// lift lands (sibling slice to this trunk PR). Method-dispatch
/// callbacks cover every cdylib-callable inherent method on
/// `CudaSurfaceAdapter` plus the `SurfaceAdapter` trait methods.
///
/// # Handle lifetime
///
/// `clone_handle` / `drop_handle` mirror the
/// `GpuContextLimitedAccessVTable` v2 pattern from
/// `streamlib-plugin-abi`: `clone_handle(borrowed) -> owned` bumps
/// the host's `Arc<CudaSurfaceAdapter<D>>` refcount;
/// `drop_handle(owned)` releases. The owned handle remains valid
/// even after the originating runtime context is dropped, which
/// matches the existing `CudaContext: Clone` contract — a plugin
/// can stash a clone in `setup()` and hand it to a worker thread
/// that outlives the lifecycle call.
///
/// # Two resource flavors, one vtable
///
/// Buffer-flavored ops (`acquire_read` / `acquire_write` /
/// `try_acquire_*`) produce a [`CudaBufferViewRepr`].
/// Image-flavored ops (`acquire_texture` / `acquire_surface` /
/// `try_acquire_*`) produce a [`CudaImageViewRepr`]. Both ride the
/// same `end_read_access` / `end_write_access` release path — the
/// adapter's registry holds the resource discriminator per
/// surface_id, so the cdylib never has to pick between two release
/// slots.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. New methods
/// append to the end and bump
/// [`CUDA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
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
pub struct CudaSurfaceAdapterVTable {
    /// Vtable layout version. Must equal
    /// [`CUDA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    // -----------------------------------------------------------------
    // Handle lifetime
    // -----------------------------------------------------------------

    /// Take a borrowed handle (typically minted by the host's
    /// runtime context when wiring the cdylib-side `CudaContext`
    /// β-shape) and return a new owned handle with an Arc refcount
    /// bump on the underlying `Arc<CudaSurfaceAdapter<D>>`. The
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
    // Registry management (inherent on CudaSurfaceAdapter)
    // -----------------------------------------------------------------

    /// Register a buffer-flavored surface with the adapter.
    ///
    /// `pixel_buffer_handle` is an
    /// `Arc::into_raw(Arc<<D::Privilege as DevicePrivilege>::Buffer>)`-shaped
    /// opaque pointer produced by the host; the host implementation
    /// bumps the Arc refcount (`Arc::increment_strong_count`) and
    /// stores a clone in the adapter's internal registry. The
    /// caller's `pixel_buffer_handle` Arc remains owned by the
    /// caller.
    ///
    /// `timeline_handle` is an
    /// `Arc::into_raw(Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore>)`-shaped
    /// opaque pointer with identical ownership semantics.
    ///
    /// `initial_layout_raw` is the i32 `VkImageLayout` enumerant
    /// — unused on the buffer path (buffers have no layout), pass
    /// `VK_IMAGE_LAYOUT_UNDEFINED` (`0`). Kept for shape parity
    /// with [`Self::register_host_image_surface`].
    ///
    /// Returns 0 on success, non-zero on failure (e.g. surface_id
    /// already registered). On error writes a UTF-8 message into
    /// `err_buf`.
    pub register_host_surface: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id: u64,
        pixel_buffer_handle: *const c_void,
        timeline_handle: *const c_void,
        initial_layout_raw: i32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Register an image-flavored surface with the adapter.
    ///
    /// `texture_handle` is an
    /// `Arc::into_raw(Arc<<D::Privilege as DevicePrivilege>::Texture>)`-shaped
    /// opaque pointer produced by the host (the host's
    /// `HostVulkanTexture::new_opaque_fd_export` output).
    /// `timeline_handle` is an
    /// `Arc::into_raw(Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore>)`-shaped
    /// opaque pointer.
    ///
    /// `initial_layout_raw` is the i32 `VkImageLayout` enumerant
    /// the image is in at registration time. Load-bearing for the
    /// cross-process release path that composes
    /// `VulkanSurfaceAdapter::release_to_foreign`; the cuda adapter
    /// itself does not issue Vulkan-side barriers on imported
    /// images (CUDA's sync runs pairwise via
    /// `cudaWaitExternalSemaphoresAsync` /
    /// `cudaSignalExternalSemaphoresAsync` on the timeline).
    ///
    /// Returns 0 on success, non-zero on failure (e.g. surface_id
    /// already registered, OR the texture's format is not in the
    /// CUDA-mappable subset — `Rgba8Unorm`, `Rgba16Float`,
    /// `Rgba32Float`). On error writes a UTF-8 message into
    /// `err_buf`.
    pub register_host_image_surface: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id: u64,
        texture_handle: *const c_void,
        timeline_handle: *const c_void,
        initial_layout_raw: i32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Drop a registered surface (either flavor). Idempotent —
    /// missing entries return 0 via `*out_was_present = 0`. Calls
    /// against a null handle return 0 with `*out_was_present = 0`.
    pub unregister_host_surface: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id: u64,
        out_was_present: *mut u32,
    ),

    /// Snapshot the adapter's registry size (number of currently-
    /// registered surfaces, either flavor). Returns 0 on null
    /// handle. Used for host-side tests and observability; cdylibs
    /// can call it today but the read is informational, not
    /// synchronizing.
    pub registered_count: unsafe extern "C" fn(handle: *const c_void) -> usize,

    // -----------------------------------------------------------------
    // SurfaceAdapter trait methods (buffer flavor)
    // -----------------------------------------------------------------

    /// Blocking read acquire (buffer flavor).
    ///
    /// `surface_ptr` is a `*const StreamlibSurface` borrowed from
    /// the caller's stack; valid for the duration of the call. On
    /// success writes the populated [`CudaBufferViewRepr`] into
    /// `*out_view` and returns 0. On contention (Ok(None) shape)
    /// the host returns 1 with a "writer contended" message; the
    /// blocking variant never returns the contended-but-not-error
    /// case (`try_acquire_*` does). Returns non-zero with an error
    /// message on any other failure (surface_id not registered,
    /// surface is image-flavored — use `acquire_texture` instead,
    /// timeline wait timed out).
    pub acquire_read: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut CudaBufferViewRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Blocking write acquire (buffer flavor). Same shape as
    /// [`Self::acquire_read`] but exclusive-write semantics.
    pub acquire_write: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut CudaBufferViewRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Non-blocking read acquire (buffer flavor).
    ///
    /// Returns 0 on success and writes `*out_acquired = 1` plus a
    /// populated view. Returns 0 with `*out_acquired = 0` on
    /// contention (Ok(None) shape) without writing the view.
    /// Returns non-zero with an error message on real failure.
    pub try_acquire_read: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut CudaBufferViewRepr,
        out_acquired: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Non-blocking write acquire (buffer flavor). Same shape as
    /// [`Self::try_acquire_read`].
    pub try_acquire_write: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut CudaBufferViewRepr,
        out_acquired: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -----------------------------------------------------------------
    // Image-flavor acquire methods
    // -----------------------------------------------------------------

    /// Blocking acquire of read-only image access — the
    /// `cudaTextureObject_t` side of CUDA's texture interop.
    ///
    /// Same shape as [`Self::acquire_read`] but produces a
    /// [`CudaImageViewRepr`] and requires the surface to be
    /// registered via [`Self::register_host_image_surface`]. Calling
    /// against a buffer-flavored surface returns non-zero with a
    /// "use acquire_read for buffer surfaces" error message.
    pub acquire_texture: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut CudaImageViewRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Blocking acquire of read-write image access — the
    /// `cudaSurfaceObject_t` side. Same shape as
    /// [`Self::acquire_texture`] but exclusive-write semantics.
    pub acquire_surface: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut CudaImageViewRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Non-blocking variant of [`Self::acquire_texture`].
    pub try_acquire_texture: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut CudaImageViewRepr,
        out_acquired: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Non-blocking variant of [`Self::acquire_surface`].
    pub try_acquire_surface: unsafe extern "C" fn(
        handle: *const c_void,
        surface_ptr: *const c_void,
        out_view: *mut CudaImageViewRepr,
        out_acquired: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -----------------------------------------------------------------
    // Release (shared between buffer + image flavors)
    // -----------------------------------------------------------------

    /// Sealed: signal the release-side timeline semaphore for a
    /// read. Called by the cdylib's `ReadGuard::drop` (buffer
    /// flavor) and `CudaTextureGuard::drop` (image flavor) — both
    /// release paths converge on the same registry-side bookkeeping
    /// keyed by surface_id. Idempotent against unknown surface IDs
    /// (the host logs and returns; no error surfaced because Drop
    /// can't propagate).
    pub end_read_access: unsafe extern "C" fn(handle: *const c_void, surface_id: u64),

    /// Sealed: signal the release-side timeline semaphore for a
    /// write. Called by the cdylib's `WriteGuard::drop` (buffer
    /// flavor) and `CudaSurfaceGuard::drop` (image flavor).
    pub end_write_access: unsafe extern "C" fn(handle: *const c_void, surface_id: u64),
}

// Safety: every field is a primitive integer or an `unsafe extern "C" fn`
// pointer; no thread-local state, no interior mutability. The host
// guarantees the pointed-at adapter state outlives every cdylib that
// holds a clone of the handle via the loader's pinning shape.
unsafe impl Send for CudaSurfaceAdapterVTable {}
unsafe impl Sync for CudaSurfaceAdapterVTable {}

// ============================================================================
// Layout regression tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    /// `VkImageLayoutValueRepr` is `#[repr(transparent)]` over i32 —
    /// 4 bytes, 4-byte aligned.
    #[test]
    fn vk_image_layout_value_repr_is_transparent_i32() {
        assert_eq!(size_of::<VkImageLayoutValueRepr>(), 4);
        assert_eq!(align_of::<VkImageLayoutValueRepr>(), 4);
    }

    /// `TextureFormatRepr` is `#[repr(transparent)]` over u32 —
    /// 4 bytes, 4-byte aligned. Mirrors the source enum's
    /// `#[repr(u32)]` representation.
    #[test]
    fn texture_format_repr_is_transparent_u32() {
        assert_eq!(size_of::<TextureFormatRepr>(), 4);
        assert_eq!(align_of::<TextureFormatRepr>(), 4);
    }

    /// `CudaBufferViewRepr` — `(vk_buffer: u64, size: u64)` =
    /// 16 bytes, align 8.
    #[test]
    fn cuda_buffer_view_repr_layout() {
        assert_eq!(offset_of!(CudaBufferViewRepr, vk_buffer), 0);
        assert_eq!(offset_of!(CudaBufferViewRepr, size), 8);
        assert_eq!(size_of::<CudaBufferViewRepr>(), 16);
        assert_eq!(align_of::<CudaBufferViewRepr>(), 8);
    }

    /// `CudaImageViewRepr` — `(vk_image: u64, width: u32, height:
    /// u32, format: u32, _padding: u32)` = 24 bytes, align 8.
    #[test]
    fn cuda_image_view_repr_layout() {
        // vk_image: u64 @ 0
        // width: u32 @ 8
        // height: u32 @ 12
        // format: TextureFormatRepr (u32) @ 16
        // _padding: u32 @ 20
        // total: 24 bytes, align 8
        assert_eq!(offset_of!(CudaImageViewRepr, vk_image), 0);
        assert_eq!(offset_of!(CudaImageViewRepr, width), 8);
        assert_eq!(offset_of!(CudaImageViewRepr, height), 12);
        assert_eq!(offset_of!(CudaImageViewRepr, format), 16);
        assert_eq!(offset_of!(CudaImageViewRepr, _padding), 20);
        assert_eq!(size_of::<CudaImageViewRepr>(), 24);
        assert_eq!(align_of::<CudaImageViewRepr>(), 8);
    }

    /// Locks the vtable's binary layout. Anchors every method slot
    /// at a fixed byte offset so a host built against vtable v1
    /// and a cdylib built against vtable v1 dispatch through the
    /// same offsets regardless of rustc-minor / dep-graph drift.
    /// New methods must append after `end_write_access` and bump
    /// [`CUDA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION`].
    #[test]
    fn cuda_surface_adapter_vtable_layout() {
        // layout_version: u32 @ 0
        // _reserved_padding: u32 @ 4
        // 16 fn pointers (8 bytes each) @ 8..136
        // total: 4 + 4 + 16*8 = 136 bytes, align 8
        assert_eq!(CUDA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION, 1);
        assert_eq!(size_of::<CudaSurfaceAdapterVTable>(), 136);
        assert_eq!(align_of::<CudaSurfaceAdapterVTable>(), 8);
        assert_eq!(offset_of!(CudaSurfaceAdapterVTable, layout_version), 0);
        assert_eq!(offset_of!(CudaSurfaceAdapterVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(CudaSurfaceAdapterVTable, clone_handle), 8);
        assert_eq!(offset_of!(CudaSurfaceAdapterVTable, drop_handle), 16);
        assert_eq!(
            offset_of!(CudaSurfaceAdapterVTable, register_host_surface),
            24
        );
        assert_eq!(
            offset_of!(CudaSurfaceAdapterVTable, register_host_image_surface),
            32
        );
        assert_eq!(
            offset_of!(CudaSurfaceAdapterVTable, unregister_host_surface),
            40
        );
        assert_eq!(offset_of!(CudaSurfaceAdapterVTable, registered_count), 48);
        assert_eq!(offset_of!(CudaSurfaceAdapterVTable, acquire_read), 56);
        assert_eq!(offset_of!(CudaSurfaceAdapterVTable, acquire_write), 64);
        assert_eq!(offset_of!(CudaSurfaceAdapterVTable, try_acquire_read), 72);
        assert_eq!(offset_of!(CudaSurfaceAdapterVTable, try_acquire_write), 80);
        assert_eq!(offset_of!(CudaSurfaceAdapterVTable, acquire_texture), 88);
        assert_eq!(offset_of!(CudaSurfaceAdapterVTable, acquire_surface), 96);
        assert_eq!(
            offset_of!(CudaSurfaceAdapterVTable, try_acquire_texture),
            104
        );
        assert_eq!(
            offset_of!(CudaSurfaceAdapterVTable, try_acquire_surface),
            112
        );
        assert_eq!(offset_of!(CudaSurfaceAdapterVTable, end_read_access), 120);
        assert_eq!(offset_of!(CudaSurfaceAdapterVTable, end_write_access), 128);
    }

    /// Compile-time witness that the vtable is `Send + Sync`. A
    /// future regression that adds an interior-mutable or non-thread-
    /// safe field would break the unsafe impl above and trip this
    /// test at build time.
    #[test]
    fn vtable_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CudaSurfaceAdapterVTable>();
    }
}
