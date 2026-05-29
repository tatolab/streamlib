// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextLimitedAccessVTable` — extern "C" dispatch for the cdylib-facing
//! `GpuContextLimitedAccess` shim.

use core::ffi::c_void;

/// Layout version of [`crate::GpuContextLimitedAccessVTable`].
///
/// Every Arc-holding return type on the cdylib-facing surface
/// (`PixelBuffer`, `Texture`, `PooledTextureHandle`, 4 Linux-only
/// buffer types, `TextureRegistration`, `RhiCommandQueue`,
/// `CommandBuffer`, `SurfaceStore`) carries its own clone/drop
/// callback pair so refcount accounting runs in host-compiled
/// code regardless of caller plugin. Method-dispatch callbacks
/// cover every cdylib-callable inherent method on
/// `GpuContextLimitedAccess`.
///
/// `CommandBuffer` and `PooledTextureHandle` are intentionally
/// NOT `Clone` — `CommandBuffer` has consume-semantics
/// `commit(self)` / `commit_and_wait(self)` (the cdylib nulls
/// the local handle/vtable fields after dispatch so Drop becomes
/// a no-op); `PooledTextureHandle::Drop` releases the underlying
/// pool slot. Linux-only callbacks ship platform stubs on other
/// triples so the vtable layout stays unconditional.
///
/// - v10: Phase C3 adds `escalate_begin` / `escalate_end` so the
///   cdylib-side `GpuContextLimitedAccess::escalate(|full| ...)` can
///   acquire the host's escalate gate, mint an opaque scope token,
///   and pair it with the FullAccess vtable for the
///   vtable-dispatched transition into `GpuContextFullAccess`.
///   Validation of the
///   scope token on every FullAccess vtable call lives on the
///   FullAccess vtable side (each callback short-circuits to
///   `Error::InvalidEscalateScope` when the token is stale).
/// - v11: Phase F (#908 / #957) adds `texture_native_dma_buf_fd`
///   for the cdylib-facing
///   [`crate::core::rhi::Texture::native_handle`] DMA-BUF FD export
///   path. Real cdylib use case: subprocess adapters that need to
///   hand a `Texture`'s DMA-BUF FD to a different GPU API (CUDA,
///   OpenGL, downstream IPC) without falling through `host_inner()`
///   and panicking. Returns the FD widened to `i64`; `-1` encodes
///   `Option::None`. Non-Linux hosts return `-1` unconditionally;
///   the macOS / Windows native-handle variants are deferred per
///   #908's AI Agent Notes.
/// - v12: #958 follow-up to #914 — adds
///   `set_video_source_timeline_semaphore` /
///   `clear_video_source_timeline_semaphore` slots. The camera
///   processor (loaded as a cdylib via `runtime.add_module`)
///   publishes its `Arc<HostVulkanTimelineSemaphore>` for in-process
///   display consumers to wait on; #971 originally panic-guarded
///   these methods on the premise no cdylib reaches them, but the
///   camera-as-cdylib lifecycle established by #914 does in fact
///   call them. Wire format mirrors the LimitedAccess Arc-borrow
///   pattern from `register_texture`: the cdylib passes
///   `Arc::as_ptr(&timeline) as *const c_void`; the host
///   `Arc::increment_strong_count` + `Arc::from_raw`s a temporary
///   borrow, calls `set_video_source_timeline_semaphore(&arc)`
///   (which itself clones into the slot), then lets the temporary
///   drop. The clear variant is a void no-arg callback. Linux-only
///   on the host side; non-Linux stubs are no-ops.
/// - v13: #958 Phase E sub — adds `wait_timeline_semaphore` slot.
///   Lets the cdylib-side `HostVulkanTimelineSemaphore::wait` —
///   used per-frame by the camera processor on its capture
///   timeline — dispatch through the host instead of touching the
///   host's `vulkanalia::Device` from cdylib code directly.
///   `timeline_handle` is `Arc::as_ptr(timeline) as *const c_void`
///   (borrowed, same shape as `set_video_source_timeline_semaphore`).
///   Returns 0 on success, non-zero (`err_buf` populated) on driver
///   failure / timeout. Linux-only on the host side.
/// - v14: #1066 — adds `host_video_source_timeline_arc` slot. The
///   read-side counterpart of v12's `set_/clear_` pair: cdylib
///   consumers (the in-tree consumer is `LinuxDisplayProcessor`'s
///   render loop) clone the host's published
///   `Arc<HostVulkanTimelineSemaphore>` across the plugin ABI to GPU-wait
///   on the producer's writeback timeline. Mirrors the v9
///   `host_vulkan_device_arc` FullAccess pattern: the host callback
///   `Arc::into_raw`s a fresh clone (or returns null when no
///   producer has published a timeline); the cdylib reconstitutes
///   via `Arc::from_raw`. Same rustc-version + dep-graph coupling
///   caveat as `set_video_source_timeline_semaphore` (in-tree
///   workspace plugins share the contract;
///   `HostVulkanTimelineSemaphore` is not `#[repr(C)]`). Linux-only
///   on the host side; non-Linux stub returns null. Null
///   `gpu_handle` returns null.
pub const GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION: u32 = 14;

/// Dispatch table for the host's `GpuContextLimitedAccess`. The
/// cdylib obtains a handle via
/// [`crate::RuntimeContextVTable::gpu_limited_access`] and reads the static
/// vtable from [`crate::HostServices::gpu_context_limited_access_vtable`].
///
/// # Handle lifetime
///
/// `clone_handle` / `drop_handle` mirror [`crate::RuntimeOpsVTable`] v2:
/// `clone_handle(borrowed) -> owned` bumps the host's
/// `Arc<GpuContext>` refcount; `drop_handle(owned)` releases. The
/// owned handle remains valid even after the originating
/// `RuntimeContext` is dropped, which matches the existing
/// `GpuContextLimitedAccess: Clone` contract that lets plugins
/// stash a clone in `setup()` and hand it to a worker thread that
/// outlives the lifecycle call.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. New methods append
/// to the end and bump
/// [`GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION`].
#[repr(C)]
pub struct GpuContextLimitedAccessVTable {
    /// Vtable layout version. Must equal
    /// [`GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    // -------------------------------------------------------------------------
    // Handle lifetime (mirrors RuntimeOpsVTable v2)
    // -------------------------------------------------------------------------

    /// Take a borrowed handle returned from
    /// [`crate::RuntimeContextVTable::gpu_limited_access`] and return a new
    /// owned handle with an Arc refcount bump on the underlying
    /// `Arc<GpuContext>`. The owned handle remains valid even after
    /// the originating `RuntimeContext` is dropped, and MUST be
    /// released exactly once via [`Self::drop_handle`].
    pub clone_handle: unsafe extern "C" fn(borrowed_handle: *const c_void) -> *const c_void,

    /// Release an owned handle previously obtained from
    /// [`Self::clone_handle`]. Calling on a null pointer is a no-op.
    /// Calling on the same owned handle twice is undefined behaviour
    /// (it would double-free the Arc refcount).
    pub drop_handle: unsafe extern "C" fn(owned_handle: *const c_void),

    // -------------------------------------------------------------------------
    // PixelBuffer return-type lifetime
    // -------------------------------------------------------------------------
    //
    // The cdylib's `PixelBuffer` is `(handle, vtable, cached POD)` where
    // `handle` is `Arc::into_raw(Arc<PixelBufferRef>)` produced by the
    // host. The cdylib never touches Arc internals directly; both
    // refcount bumps (Clone) and decrements (Drop) dispatch through
    // these host-resident callbacks so the Arc accounting is done by
    // host-compiled code under any rustc-minor / dep-graph drift.

    /// Bump the refcount on a `PixelBuffer` handle. Called by the
    /// cdylib's `Clone for PixelBuffer`. The handle pointer is
    /// `Arc::into_raw(Arc<PixelBufferRef>)`-shaped; host
    /// implementation calls `Arc::increment_strong_count(handle)`.
    /// Calling on a null pointer is a no-op.
    pub clone_pixel_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `PixelBuffer` handle. Called by
    /// the cdylib's `Drop for PixelBuffer`. Host implementation
    /// calls `Arc::decrement_strong_count(handle)`; when the
    /// refcount reaches zero the underlying `PixelBufferRef` (and
    /// its platform buffer) is dropped. Calling on a null pointer
    /// is a no-op.
    pub drop_pixel_buffer: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // PixelBuffer method-dispatch (eliminates plugin ABI Arc::from_raw)
    // -------------------------------------------------------------------------
    //
    // The remaining non-cached `PixelBuffer` methods dispatch through
    // host-compiled code so the cdylib never casts the opaque handle
    // to a concrete `*const PixelBufferRef`. Casting the handle
    // cdylib-side would require both sides to agree on
    // `PixelBufferRef`'s in-memory layout — which the cdylib has no
    // way to guarantee under rustc-minor / dep-graph drift.

    /// Number of `PixelBuffer` references to the same underlying
    /// `PixelBufferRef`. Engine-internal probe used by the host's
    /// pool manager to detect "buffer no longer in use" without
    /// locking. Cdylib callers technically can call it through the
    /// vtable today, but the engine restricts the cdylib-facing
    /// `PixelBuffer::strong_count` API to `pub(crate)` so the
    /// plugin ABI path is host-only by visibility. Calling on a null
    /// pointer returns `0`.
    pub strong_count_pixel_buffer: unsafe extern "C" fn(handle: *const c_void) -> usize,

    /// Mapped base address for the given plane, or null if out of
    /// range. Plane 0 on a VMA-allocated or single-plane-imported
    /// buffer points at the same bytes as
    /// `slpn_gpu_surface_plane_base_address` / equivalent. Calling
    /// on a null handle returns `null`.
    pub plane_base_address_pixel_buffer:
        unsafe extern "C" fn(handle: *const c_void, plane_index: u32) -> *mut u8,

    /// Byte size of the given plane, or `0` if out of range. Calling
    /// on a null handle returns `0`.
    pub plane_size_pixel_buffer:
        unsafe extern "C" fn(handle: *const c_void, plane_index: u32) -> u64,

    // -------------------------------------------------------------------------
    // Texture return-type lifetime
    // -------------------------------------------------------------------------
    //
    // The cdylib's `Texture` is `(handle, vtable, cached POD)` where
    // `handle` is `Arc::into_raw(Arc<TextureInner>)` produced by the
    // host. Identical Arc-lifetime contract as `PixelBuffer` —
    // refcount accounting runs in host-compiled code so the cdylib
    // never has to know `TextureInner`'s layout.

    /// Bump the refcount on a `Texture` handle. Called by the
    /// cdylib's `Clone for Texture`. The handle pointer is
    /// `Arc::into_raw(Arc<TextureInner>)`-shaped; host implementation
    /// calls `Arc::increment_strong_count(handle)`. Calling on a null
    /// pointer is a no-op.
    pub clone_texture: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `Texture` handle. Called by the
    /// cdylib's `Drop for Texture`. Host implementation calls
    /// `Arc::decrement_strong_count(handle)`; when the refcount
    /// reaches zero the underlying `TextureInner` (and its platform
    /// texture) is dropped. Calling on a null pointer is a no-op.
    pub drop_texture: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // PooledTextureHandle return-type lifetime (v4 — drop-only)
    // -------------------------------------------------------------------------
    //
    // `PooledTextureHandle` is deliberately NOT `Clone`: Drop must
    // release the pool slot exactly once via the underlying
    // `TexturePoolInner::release(slot_id)` path. The cdylib carries a
    // `Box::into_raw(Box::new(PooledTextureHandleInner))`-shaped
    // handle and fires `drop_pooled_texture_handle` from its `Drop`
    // impl. There is no `clone_pooled_texture_handle` — cloning would
    // duplicate the raw pointer and double-release the slot.

    /// Release the host-side `PooledTextureHandleInner` backing a
    /// `PooledTextureHandle`. The host runs `Box::from_raw + drop`,
    /// which fires the inner's `Drop` impl and releases the pool
    /// slot. Calling on a null pointer is a no-op; calling twice on
    /// the same owned handle is undefined behaviour (double-free of
    /// the Box plus a double-release of the pool slot).
    pub drop_pooled_texture_handle: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // Method dispatch — Texture-related
    // -------------------------------------------------------------------------
    //
    // The six methods on the cdylib's `GpuContextLimitedAccess` that
    // touch `Texture` / `PooledTextureHandle` / `TextureRegistration`
    // now dispatch through these callbacks instead of through the
    // cdylib's view of `GpuContext`'s layout. Each callback's first
    // argument is the `*const Arc<GpuContext>`-shaped handle from
    // `RuntimeContextVTable::gpu_limited_access` (or a clone via
    // `Self::clone_handle`).

    /// Register a texture in the host's same-process texture cache.
    /// `texture_handle` is the `*const Arc<TextureInner>` from a
    /// cdylib-side `Texture`'s `handle` field; the host bumps the Arc
    /// refcount (`Arc::increment_strong_count`) and inserts a clone
    /// into the cache. The cdylib's caller still owns its `Texture`
    /// value and continues to be responsible for its eventual Drop.
    /// Calling with a null handle or null texture_handle is a no-op.
    ///
    /// `initial_layout_raw` is i32-encoded `VulkanLayout` on Linux;
    /// non-Linux hosts ignore the layout. The "without layout" form
    /// of the call passes `VulkanLayout::UNDEFINED` (i32 0).
    pub register_texture: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        texture_handle: *const c_void,
        initial_layout_raw: i32,
    ),

    /// Update a registered texture's tracked layout after a
    /// transition. Linux-only contract on the host side; non-Linux
    /// hosts treat this as a no-op. Calling on a missing id is a
    /// no-op.
    pub update_texture_registration_layout: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        layout_raw: i32,
    ),

    /// Acquire a pooled texture for the given descriptor. On success
    /// writes a new `PooledTextureHandle` into `*out_pooled_handle`
    /// and returns `0`. On failure writes a UTF-8 error message into
    /// `err_buf` (clamped to `err_buf_cap`), sets `*err_len` to the
    /// bytes written, and returns non-zero.
    ///
    /// The `format_raw` is the `#[repr(u32)]` discriminant of
    /// [`streamlib_consumer_rhi::TextureFormat`]; `usage_bits` is
    /// [`streamlib_consumer_rhi::TextureUsages::bits`].
    pub acquire_texture: unsafe extern "C" fn(
        handle: *const c_void,
        width: u32,
        height: u32,
        format_raw: u32,
        usage_bits: u32,
        out_pooled_handle: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Resolve a VideoFrame's texture from its surface_id. On
    /// success writes a new `Texture` into `*out_texture` and
    /// returns `0`. On failure writes a UTF-8 error message into
    /// `err_buf` and returns non-zero.
    ///
    /// `has_layout` is `1` when `layout_raw` carries a per-frame
    /// `texture_layout` override, `0` for the default-resolution
    /// path. `width` / `height` are required for the Path 3 fallback.
    pub resolve_texture_by_surface_id: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id_ptr: *const u8,
        surface_id_len: usize,
        has_layout: i32,
        layout_raw: i32,
        width: u32,
        height: u32,
        out_texture: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Remove an id from the host's same-process texture cache.
    /// Idempotent — missing entries are a no-op.
    pub unregister_texture: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
    ),

    // -------------------------------------------------------------------------
    // Linux-only buffer Arc-handle lifecycle
    // -------------------------------------------------------------------------
    //
    // The cdylib's `StorageBuffer` / `UniformBuffer` / `VertexBuffer` /
    // `IndexBuffer` are each `(handle, vtable, byte_size, mapped_ptr)`
    // where `handle` is `Arc::into_raw(Arc<HostVulkanBuffer>)`. All
    // four wrap the same Arc type under the hood but keep separate
    // Rust newtypes for binding-shape enforcement. Each gets its own
    // clone/drop pair so the vtable structure mirrors the type
    // structure — future-proofs against per-type divergence (a
    // buffer growing extra state) without re-versioning a shared
    // callback. Stub on non-Linux hosts; callable only from cdylib
    // code that links the Linux-only buffer types.

    /// Bump the refcount on a `StorageBuffer` handle.
    /// `Arc::increment_strong_count(handle as *const HostVulkanBuffer)`.
    pub clone_storage_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `StorageBuffer` handle.
    pub drop_storage_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Bump the refcount on a `UniformBuffer` handle.
    pub clone_uniform_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `UniformBuffer` handle.
    pub drop_uniform_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Bump the refcount on a `VertexBuffer` handle.
    pub clone_vertex_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `VertexBuffer` handle.
    pub drop_vertex_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Bump the refcount on an `IndexBuffer` handle.
    pub clone_index_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on an `IndexBuffer` handle.
    pub drop_index_buffer: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // Linux-only buffer acquire methods
    // -------------------------------------------------------------------------
    //
    // Each acquire callback writes a fresh `{Storage,Uniform,Vertex,
    // Index}Buffer` into `*out_buffer` on success and returns 0; on
    // failure writes a UTF-8 message into `err_buf` and returns
    // non-zero. Non-Linux stubs return non-zero with a
    // "buffer-type-not-available-on-this-platform" message.

    /// Acquire a `StorageBuffer` of the given byte size.
    pub acquire_storage_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        byte_size: u64,
        out_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Acquire a `UniformBuffer` of the given byte size.
    pub acquire_uniform_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        byte_size: u64,
        out_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Acquire a `VertexBuffer` of the given byte size.
    pub acquire_vertex_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        byte_size: u64,
        out_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Acquire an `IndexBuffer` of the given byte size.
    pub acquire_index_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        byte_size: u64,
        out_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // TextureRegistration Arc-handle lifecycle
    // -------------------------------------------------------------------------
    //
    // The cdylib's `TextureRegistration` is `(handle, vtable)` where
    // `handle` is `Arc::into_raw(Arc<TextureRegistrationInner>)`. Same
    // shape as `PixelBuffer` / `Texture`'s Arc-handle pattern. Cdylibs
    // get Arc semantics (cheap Clone via refcount bump) without ever
    // touching the host's `Arc<T>` implementation.

    /// Bump the refcount on a `TextureRegistration` handle. Host
    /// implementation runs
    /// `Arc::increment_strong_count(handle as *const TextureRegistrationInner)`.
    pub clone_texture_registration: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `TextureRegistration` handle. When
    /// the strong count reaches zero the underlying
    /// `TextureRegistrationInner` (and its `Texture` plus
    /// `current_layout` atomic) is dropped.
    pub drop_texture_registration: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // TextureRegistration method dispatch
    // -------------------------------------------------------------------------

    /// Borrow the registration's underlying `Texture`. Returns a
    /// pointer into the host's heap allocation that is alive for as
    /// long as the caller holds the originating
    /// `TextureRegistration` (the Arc's strong count keeps the
    /// inner alive). The returned `Texture` is itself a layout-stable
    /// `#[repr(C)]` value (see `core/rhi/texture.rs::Texture::tests::texture_layout`),
    /// so the cdylib can deref the pointer directly.
    pub texture_registration_texture:
        unsafe extern "C" fn(handle: *const c_void) -> *const c_void,

    /// Last-known `VkImageLayout` (raw `i32` enumerant). Atomic
    /// `Acquire` load on the host side. Linux-only behaviour; non-
    /// Linux hosts return `0` (VK_IMAGE_LAYOUT_UNDEFINED).
    pub texture_registration_current_layout:
        unsafe extern "C" fn(handle: *const c_void) -> i32,

    /// Record a new last-known layout. Atomic `Release` store on the
    /// host side. Linux-only behaviour; non-Linux hosts treat this
    /// as a no-op.
    pub texture_registration_update_layout:
        unsafe extern "C" fn(handle: *const c_void, layout_raw: i32),

    /// Resolve a VideoFrame's full registration record (texture +
    /// layout) from its `surface_id`. On success writes a new
    /// `TextureRegistration` into `*out_registration` and returns
    /// `0`. On failure writes a UTF-8 error message into `err_buf`
    /// and returns non-zero.
    ///
    /// `has_layout` is `1` when `layout_raw` carries a per-frame
    /// `texture_layout` override, `0` for the default-resolution
    /// path. `width` / `height` are required for the Path 3 fallback.
    pub resolve_texture_registration_by_surface_id: unsafe extern "C" fn(
        handle: *const c_void,
        surface_id_ptr: *const u8,
        surface_id_len: usize,
        has_layout: i32,
        layout_raw: i32,
        width: u32,
        height: u32,
        out_registration: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // RhiCommandQueue Arc-handle lifecycle + create_command_buffer
    // -------------------------------------------------------------------------
    //
    // The cdylib's `RhiCommandQueue` is `(handle, vtable)` where
    // `handle` is `Arc::into_raw(Arc<RhiCommandQueueInner>)`. Same
    // shape as every other Arc-handle β-reshape on this vtable.

    /// Bump the refcount on an `RhiCommandQueue` handle.
    pub clone_rhi_command_queue: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on an `RhiCommandQueue` handle.
    pub drop_rhi_command_queue: unsafe extern "C" fn(handle: *const c_void),

    /// Create a new `CommandBuffer` from a queue. On success writes a
    /// fresh `CommandBuffer` (Box-handle PluginAbiObject) into `*out_cb` and
    /// returns 0; on failure writes a UTF-8 error message into
    /// `err_buf` and returns non-zero.
    pub create_command_buffer_from_queue: unsafe extern "C" fn(
        queue_handle: *const c_void,
        out_cb: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // CommandBuffer lifecycle — drop + consume-semantics commits
    // -------------------------------------------------------------------------
    //
    // `CommandBuffer` is single-use. Box-handle (not Arc) — no Clone.
    // `commit` and `commit_and_wait` are consume-semantics: the host
    // runs `Box::from_raw + commit + drop` and the cdylib nulls its
    // local handle/vtable fields so Drop becomes a no-op afterward.

    /// Release the host-side `Box<CommandBufferInner>` backing a
    /// `CommandBuffer`. Calling on a null pointer is a no-op.
    /// Calling twice on the same handle is undefined behaviour
    /// (double-free of the Box).
    pub drop_command_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Commit the command buffer for execution (consume-semantics).
    /// Host runs `Box::from_raw + commit + drop`; the cdylib's Drop
    /// is then a no-op (handle/vtable are nulled by the cdylib-side
    /// `commit(self)` wrapper).
    pub commit_command_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Commit and wait for completion (consume-semantics). Same
    /// lifetime contract as [`Self::commit_command_buffer`].
    pub commit_and_wait_command_buffer: unsafe extern "C" fn(handle: *const c_void),

    /// Copy one texture to another. `src` / `dst` are
    /// `*const Texture` pointers — the layout is locked by the
    /// per-type `texture_layout` regression test so the host's read
    /// agrees with the cdylib's write.
    pub copy_texture_command_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        src: *const c_void,
        dst: *const c_void,
    ),

    // -------------------------------------------------------------------------
    // GpuContextLimitedAccess command-queue / command-buffer / blit methods
    // -------------------------------------------------------------------------

    /// Return an owned `RhiCommandQueue` view of the host's shared
    /// command queue (refcount bumped on the underlying
    /// `Arc<RhiCommandQueueInner>`). Cdylib's caller releases via
    /// `drop_rhi_command_queue`. Writes the PluginAbiObject into
    /// `*out_queue`; returns 0 on success, non-zero on internal
    /// failure (e.g. null gpu handle).
    pub command_queue: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        out_queue: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Create a CPU-side command buffer from the shared queue. Same
    /// shape as [`Self::create_command_buffer_from_queue`] but takes
    /// a `GpuContext` handle rather than a queue handle —
    /// `GpuContextLimitedAccess::create_command_buffer` is a
    /// convenience that delegates to the engine's shared queue.
    pub create_command_buffer: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        out_cb: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Copy a host-visible pixel buffer's contents into a
    /// pre-allocated device-local texture. Linux-only on the host
    /// side; non-Linux stubs return non-zero. `pixel_buffer` and
    /// `texture` are `*const PixelBuffer` / `*const Texture` PluginAbiObject
    /// pointers.
    pub copy_pixel_buffer_to_texture: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        pixel_buffer: *const c_void,
        texture: *const c_void,
        surface_id_ptr: *const u8,
        surface_id_len: usize,
        width: u32,
        height: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Copy pixels between same-format, same-size pixel buffers.
    pub blit_copy: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        src: *const c_void,
        dst: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Copy from raw IOSurface to a pixel buffer (macOS-only). The
    /// `src_iosurface_ref` is an `IOSurfaceRef` (raw `*const c_void`).
    /// Non-macOS hosts return non-zero with a "not available on this
    /// platform" message.
    pub blit_copy_iosurface: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        src_iosurface_ref: *const c_void,
        dst_pixel_buffer: *const c_void,
        width: u32,
        height: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // SurfaceStore accessors
    // -------------------------------------------------------------------------
    //
    // The bulk of the SurfaceStore ABI lives on its own
    // SurfaceStoreVTable; these two callbacks bridge from
    // GpuContextLimitedAccess to that subsystem.

    /// Return an owned [`SurfaceStore`] PluginAbiObject if the host has one,
    /// or a null-handle PluginAbiObject ("None") otherwise. Always returns 0;
    /// callers branch on whether the written `SurfaceStore`'s handle
    /// is null. Writes a fresh PluginAbiObject (Arc refcount bumped) into
    /// `*out_store`.
    pub surface_store: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        out_store: *mut c_void,
    ),

    /// Convenience method: check out a surface from the engine's
    /// `SurfaceStore` by `surface_id` (assumes the store exists).
    /// Writes a fresh `PixelBuffer` PluginAbiObject into `*out_pixel_buffer`
    /// on success.
    pub check_out_surface: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        out_pixel_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // PixelBuffer acquire / get / resolve method-dispatch
    // -------------------------------------------------------------------------

    /// Acquire a pixel buffer from a pre-reserved pool. The tuple
    /// return `(PixelBufferPoolId, PixelBuffer)` is encoded via
    /// paired out-params: `out_pool_id_buf` receives the
    /// `PixelBufferPoolId`'s string bytes (capped at
    /// `out_pool_id_cap`; `*out_pool_id_len` receives the actual
    /// length, truncated to fit). `*out_pixel_buffer` receives a
    /// fresh `PixelBuffer` PluginAbiObject on success.
    ///
    /// `format_raw` is the `#[repr(u32)]` discriminant of
    /// [`streamlib_consumer_rhi::PixelFormat`].
    pub acquire_pixel_buffer: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        width: u32,
        height: u32,
        format_raw: u32,
        out_pool_id_buf: *mut u8,
        out_pool_id_cap: usize,
        out_pool_id_len: *mut usize,
        out_pixel_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Get a pixel buffer by its pool id (local-cache fast path).
    /// `pool_id_ptr` / `pool_id_len` is the UTF-8 byte
    /// representation of the `PixelBufferPoolId`'s inner string.
    pub get_pixel_buffer: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        pool_id_ptr: *const u8,
        pool_id_len: usize,
        out_pixel_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Resolve a VideoFrame's buffer from its `surface_id`.
    pub resolve_pixel_buffer_by_surface_id: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        surface_id_ptr: *const u8,
        surface_id_len: usize,
        out_pixel_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Escalate scope transition (Phase C3)
    // -------------------------------------------------------------------------

    /// Begin an escalate scope. Acquires the host's escalate gate on
    /// the supplied `gpu_handle`, mints an opaque scope token, and
    /// writes it into `*out_scope_token` on success. Returns 0 on
    /// success, non-zero on failure (message in `err_buf`).
    ///
    /// The token is opaque to the caller; the cdylib's
    /// [`GpuContextLimitedAccess::escalate`] wrapper passes it as the
    /// `gpu_handle` slot when constructing a cdylib-side
    /// [`GpuContextFullAccess`] and back to `escalate_end` when the
    /// scope completes. Every FullAccess vtable callback validates
    /// the token against the host's
    /// `escalate_scope_registry` before dispatch; calls after
    /// `escalate_end` (or against a never-issued token) return a
    /// `Error::InvalidEscalateScope`-flavored error in the callback's
    /// `err_buf`.
    ///
    /// Blocking: the gate's `enter` serializes against any other
    /// escalate scope on the same `GpuContext` (host-mode or
    /// cdylib-mode), so `escalate_begin` may block for the duration
    /// of a prior scope.
    ///
    /// [`GpuContextLimitedAccess::escalate`]: streamlib_plugin_abi::GpuContextLimitedAccessVTable
    /// [`GpuContextFullAccess`]: streamlib_plugin_abi::GpuContextFullAccessVTable
    pub escalate_begin: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        out_scope_token: *mut *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// End an escalate scope. Releases the host's escalate gate and
    /// invalidates the token, then waits for the GPU device to go
    /// idle (matching the host-mode escalate path's
    /// `wait_device_idle` at scope end). Returns 0 on success,
    /// non-zero on failure (message in `err_buf`); a non-zero return
    /// indicates `wait_device_idle` failed — the scope is still
    /// invalidated and the gate is still released.
    ///
    /// Idempotent against a never-issued or already-ended token —
    /// the call returns 0 cleanly without releasing another scope's
    /// gate.
    pub escalate_end: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        scope_token: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Texture method dispatch — DMA-BUF FD export (Phase F)
    // -------------------------------------------------------------------------

    /// Export the texture's GPU memory as a Linux DMA-BUF file
    /// descriptor for cross-framework / cross-process sharing.
    ///
    /// **The returned FD is borrowed — owned by the host-side
    /// texture for as long as the cdylib-side [`Texture`] keeps its
    /// `Arc<TextureInner>` strong count > 0.** The host caches the
    /// FD on the first call and closes it in `HostVulkanTexture`'s
    /// `Drop`; callers that need to hand the FD to a different
    /// process or to an API that consumes it (e.g.
    /// `vkImportMemoryFdKHR` takes ownership on success) MUST
    /// `dup(2)` the returned FD first.
    ///
    /// `texture_handle` is the `*const Arc<TextureInner>`-shaped
    /// `handle` field on a cdylib-side [`Texture`] (the same handle
    /// the cdylib already passes to `clone_texture` / `drop_texture`
    /// — see [`crate::core::rhi::Texture`]). The host derefs it as a
    /// borrow without touching the refcount, calls the platform-
    /// specific Vulkan export path, and returns the FD via the i64
    /// return value.
    ///
    /// Encoding:
    ///   - `>= 0` — valid DMA-BUF FD (always fits in `i32` on Linux,
    ///     widened to `i64` for forward-compat with any future
    ///     platform that exposes wider FD-like identifiers via the
    ///     same slot).
    ///   - `-1` — texture has no DMA-BUF FD (not Linux, or no Vulkan
    ///     backing, or export failed). Equivalent to `Option::None`
    ///     on the cdylib-facing
    ///     [`crate::core::rhi::Texture::native_handle`].
    ///
    /// Non-Linux hosts return `-1` unconditionally — DMA-BUF is a
    /// Linux concept; macOS IOSurface and Windows DXGI shared handles
    /// are deferred to future slots when their respective cdylib
    /// adapter work resumes (see #908's deferred macOS list).
    ///
    /// Calling with a null `texture_handle` returns `-1` (no panic).
    pub texture_native_dma_buf_fd:
        unsafe extern "C" fn(texture_handle: *const c_void) -> i64,

    // -------------------------------------------------------------------------
    // Video-source timeline semaphore publish/clear (v12 — #958)
    // -------------------------------------------------------------------------

    /// Publish a producer's `Arc<HostVulkanTimelineSemaphore>` for
    /// in-process GPU-GPU sync (the in-tree consumer is
    /// `LinuxDisplayProcessor::render_frame`, which waits on the
    /// camera's published timeline before binding the captured
    /// texture).
    ///
    /// `timeline_handle` is `Arc::as_ptr(timeline) as *const c_void`
    /// — a **borrowed** pointer; the cdylib retains its own Arc and
    /// the host does NOT consume the caller's reference. The host
    /// callback `Arc::increment_strong_count`s the pointer,
    /// reconstitutes a temporary owned Arc via `Arc::from_raw`,
    /// calls `gpu.set_video_source_timeline_semaphore(&arc)` (which
    /// itself clones into the slot), and lets the temporary Arc
    /// drop — net effect: one fresh strong count moves into the
    /// slot; the cdylib's Arc is unchanged.
    ///
    /// Mirrors the Arc-borrow-+-strong-count-bump pattern
    /// [`Self::register_texture`] uses for `Arc<TextureInner>`.
    ///
    /// **Arc-raw-pointer transit** — not a layout-stable PluginAbiObject.
    /// In-tree consumers (camera) ride this freely because they're
    /// built in the same workspace as the engine. Cross-repo plugin
    /// distribution will need a PluginAbiObject lift for
    /// `HostVulkanTimelineSemaphore`; tracked as a future follow-up
    /// alongside `create_timeline_semaphore`'s identical caveat.
    ///
    /// Linux-only on the host side; non-Linux stubs are no-ops.
    /// Calling with a null `gpu_handle` or null `timeline_handle` is
    /// a no-op.
    pub set_video_source_timeline_semaphore: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        timeline_handle: *const c_void,
    ),

    /// Drop the published producer timeline so consumers observe the
    /// absence and skip the wait. Idempotent against a never-set or
    /// already-cleared slot. Pairs with
    /// [`Self::set_video_source_timeline_semaphore`].
    ///
    /// Linux-only on the host side; non-Linux stubs are no-ops.
    /// Calling with a null `gpu_handle` is a no-op.
    pub clear_video_source_timeline_semaphore: unsafe extern "C" fn(
        gpu_handle: *const c_void,
    ),

    // -------------------------------------------------------------------------
    // HostVulkanTimelineSemaphore::wait (v13 — #958 Phase E sub)
    // -------------------------------------------------------------------------

    /// Block until the host's `HostVulkanTimelineSemaphore` counter
    /// has reached or surpassed `value`. Called per-frame from
    /// `HostVulkanTimelineSemaphore::wait` on the cdylib side; the
    /// host calls `vkWaitSemaphores` against its own loaded
    /// `vulkanalia::Device` to avoid running Vulkan dispatch from a
    /// statically-linked cdylib copy of the loader.
    ///
    /// `timeline_handle` is a borrowed `*const HostVulkanTimelineSemaphore`
    /// — the cdylib-side `wait` method takes `&self` and passes
    /// `self as *const Self as *const c_void`; when the caller
    /// instead holds an `Arc<HostVulkanTimelineSemaphore>` directly
    /// (rare for `wait`), `Arc::as_ptr(&arc)` resolves to the same
    /// borrow pointer. The host does NOT bump the refcount on the
    /// borrow.
    ///
    /// `timeout_ns` is the per-call timeout; pass `u64::MAX` for
    /// no timeout. Returns 0 on success, non-zero (`err_buf`
    /// populated) on driver failure / timeout. Null `gpu_handle` or
    /// null `timeline_handle` writes a "null handle" error and
    /// returns non-zero.
    ///
    /// Linux-only on the host side; non-Linux stubs return non-zero.
    pub wait_timeline_semaphore: unsafe extern "C" fn(
        gpu_handle: *const c_void,
        timeline_handle: *const c_void,
        value: u64,
        timeout_ns: u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // host_video_source_timeline_arc (v14 — #1066)
    // -------------------------------------------------------------------------

    /// Clone the host's `Arc<HostVulkanTimelineSemaphore>` that was
    /// most recently published via
    /// [`Self::set_video_source_timeline_semaphore`], so a cdylib
    /// consumer can GPU-wait on it without reaching engine-internal
    /// state. The in-tree consumer is `LinuxDisplayProcessor`'s
    /// render loop (waits on the camera's published timeline before
    /// binding the captured texture).
    ///
    /// Mirrors the v9 `host_vulkan_device_arc` FullAccess shape:
    /// host callback `Arc::into_raw`s a fresh clone of the
    /// `Arc<HostVulkanTimelineSemaphore>` from the slot; cdylib
    /// reconstitutes via `Arc::from_raw`. The fresh strong count
    /// moves into the cdylib's Arc; the host's slot keeps its own
    /// independent strong count.
    ///
    /// Returns `*const c_void` carrying the leaked Arc pointer (a
    /// `*const HostVulkanTimelineSemaphore` widened to the plugin ABI
    /// type), or null when no producer has published a timeline
    /// yet, when the slot was cleared via
    /// [`Self::clear_video_source_timeline_semaphore`], when
    /// `gpu_handle` is null, or on non-Linux hosts.
    ///
    /// **Arc-raw-pointer transit** — same rustc-version coupling
    /// caveat as `set_video_source_timeline_semaphore`.
    /// `HostVulkanTimelineSemaphore` is not `#[repr(C)]`; in-tree
    /// workspace plugin cdylibs share the host's rustc + dep graph
    /// and ride this freely. Cross-repo plugin distribution awaits
    /// a PluginAbiObject lift of `HostVulkanTimelineSemaphore` (the same
    /// dormant work the v12 set/clear pair flagged).
    ///
    /// Linux-only on the host side; non-Linux stubs return null.
    pub host_video_source_timeline_arc: unsafe extern "C" fn(
        gpu_handle: *const c_void,
    ) -> *const c_void,
}

unsafe impl Send for GpuContextLimitedAccessVTable {}
unsafe impl Sync for GpuContextLimitedAccessVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn gpu_context_limited_access_vtable_layout() {
        // layout_version (u32) + _reserved_padding (u32) + 57 fn
        // pointers (8 bytes each) = 4 + 4 + 456 = 464 bytes, align = 8.
        assert_eq!(size_of::<GpuContextLimitedAccessVTable>(), 464);
        assert_eq!(align_of::<GpuContextLimitedAccessVTable>(), 8);
        assert_eq!(offset_of!(GpuContextLimitedAccessVTable, layout_version), 0);
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, _reserved_padding),
            4
        );
        assert_eq!(offset_of!(GpuContextLimitedAccessVTable, clone_handle), 8);
        assert_eq!(offset_of!(GpuContextLimitedAccessVTable, drop_handle), 16);
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_pixel_buffer),
            24
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_pixel_buffer),
            32
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, strong_count_pixel_buffer),
            40
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, plane_base_address_pixel_buffer),
            48
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, plane_size_pixel_buffer),
            56
        );
        assert_eq!(offset_of!(GpuContextLimitedAccessVTable, clone_texture), 64);
        assert_eq!(offset_of!(GpuContextLimitedAccessVTable, drop_texture), 72);
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_pooled_texture_handle),
            80
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, register_texture),
            88
        );
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                update_texture_registration_layout
            ),
            96
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, acquire_texture),
            104
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, resolve_texture_by_surface_id),
            112
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, unregister_texture),
            120
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_storage_buffer),
            128
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_storage_buffer),
            136
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_uniform_buffer),
            144
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_uniform_buffer),
            152
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_vertex_buffer),
            160
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_vertex_buffer),
            168
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_index_buffer),
            176
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_index_buffer),
            184
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, acquire_storage_buffer),
            192
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, acquire_uniform_buffer),
            200
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, acquire_vertex_buffer),
            208
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, acquire_index_buffer),
            216
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_texture_registration),
            224
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_texture_registration),
            232
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, texture_registration_texture),
            240
        );
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                texture_registration_current_layout
            ),
            248
        );
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                texture_registration_update_layout
            ),
            256
        );
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                resolve_texture_registration_by_surface_id
            ),
            264
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, clone_rhi_command_queue),
            272
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_rhi_command_queue),
            280
        );
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                create_command_buffer_from_queue
            ),
            288
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, drop_command_buffer),
            296
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, commit_command_buffer),
            304
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, commit_and_wait_command_buffer),
            312
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, copy_texture_command_buffer),
            320
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, command_queue),
            328
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, create_command_buffer),
            336
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, copy_pixel_buffer_to_texture),
            344
        );
        assert_eq!(offset_of!(GpuContextLimitedAccessVTable, blit_copy), 352);
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, blit_copy_iosurface),
            360
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, surface_store),
            368
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, check_out_surface),
            376
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, acquire_pixel_buffer),
            384
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, get_pixel_buffer),
            392
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, resolve_pixel_buffer_by_surface_id),
            400
        );
        // C3-added entries (Phase C3, #903).
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, escalate_begin),
            408
        );
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, escalate_end),
            416
        );
        // Phase F entry (#908 / #957).
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, texture_native_dma_buf_fd),
            424
        );
        // v12 entries (#958).
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                set_video_source_timeline_semaphore
            ),
            432
        );
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                clear_video_source_timeline_semaphore
            ),
            440
        );
        // v13 entry (#958 Phase E sub).
        assert_eq!(
            offset_of!(GpuContextLimitedAccessVTable, wait_timeline_semaphore),
            448
        );
        // v14 entry (#1066).
        assert_eq!(
            offset_of!(
                GpuContextLimitedAccessVTable,
                host_video_source_timeline_arc
            ),
            456
        );
    }
}
