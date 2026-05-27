// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `GpuContextLimitedAccessVTable` + `GpuContextFullAccessVTable`
//! callbacks + static vtables + accessors.
//!
//! The LimitedAccess vtable is the cdylib-facing surface for sandboxed
//! GPU work; the FullAccess vtable is reached only inside
//! `escalate(|full| ...)` scopes via the LimitedAccess vtable's
//! `escalate_begin` callback. Every body deref's the opaque `handle`
//! pointer back to a host-owned Rust type (`Arc<GpuContext>` for
//! Limited; a `ScopeToken` for Full).

use std::ffi::c_void;
use std::sync::Arc;

use streamlib_plugin_abi::{
    ComputeKernelDescriptorRepr, GpuContextFullAccessVTable, GpuContextLimitedAccessVTable,
    GraphicsKernelDescriptorRepr, RayTracingKernelDescriptorRepr,
    GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION,
    GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION,
};

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::{slice_from_raw, write_err, write_id_bytes};

mod scope_token;
mod shared;

use scope_token::with_full_scope_or_err;
use shared::{handle_as_gpu_context, pixel_format_from_raw};

// pointers and reading nothing about layout.

// ---------------- GpuContextLimitedAccess vtable ----------------
//
// Host-side implementations of every callback on the
// [`GpuContextLimitedAccessVTable`]. The static at the bottom of
// this block (`HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE`) wires them
// up; the cdylib-side mirror lives in the cdylib's statically-
// linked engine copy and reads through the host-installed pointer
// on [`HostServices::gpu_context_limited_access_vtable`].

unsafe extern "C" fn host_gpu_lim_clone_handle(borrowed_handle: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_gpu_lim_clone_handle",
        || {
            if borrowed_handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: `borrowed_handle` was produced by
            // `GpuContextLimitedAccess::new` (or a prior
            // `clone_handle`) as
            // `Box::into_raw(Box::new(Arc::new(GpuContext)))`.
            // Reading through `&*` and cloning the Arc bumps the
            // underlying refcount; we re-leak via
            // `Box::into_raw(Box::new(...))` so the caller gets a
            // fresh owned handle that matches `drop_handle`'s
            // expected shape.
            let original =
                unsafe { &*(borrowed_handle as *const std::sync::Arc<crate::core::context::GpuContext>) };
            Box::into_raw(Box::new(original.clone())) as *const c_void
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_handle(owned_handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_handle",
        || {
            if owned_handle.is_null() {
                return;
            }
            // SAFETY: paired with `GpuContextLimitedAccess::new` and
            // `host_gpu_lim_clone_handle` — both produce
            // `Box::into_raw(Box::new(Arc<GpuContext>))`. Reclaiming
            // via `Box::from_raw` drops the Arc, which decrements
            // the host's `Arc<GpuContext>` refcount and frees the
            // underlying `GpuContext` when the count reaches zero.
            unsafe {
                let _ = Box::from_raw(
                    owned_handle as *mut std::sync::Arc<crate::core::context::GpuContext>,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clone_pixel_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_pixel_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is a `*const c_void` cast of
            // `Arc::into_raw(Arc<PixelBufferRef>)` produced by
            // `PixelBuffer::new` (host-side). Re-interpreting it as
            // `*const PixelBufferRef` and bumping the strong count is the
            // documented `Arc::increment_strong_count` contract.
            unsafe {
                Arc::increment_strong_count(handle as *const crate::core::rhi::PixelBufferRef);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_pixel_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_pixel_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `host_gpu_lim_clone_pixel_buffer` and
            // `PixelBuffer::new`'s `Arc::into_raw` initial bump.
            // `Arc::decrement_strong_count` decrements; when refcount hits
            // zero the underlying `PixelBufferRef` is dropped along with
            // its platform buffer.
            unsafe {
                Arc::decrement_strong_count(handle as *const crate::core::rhi::PixelBufferRef);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_strong_count_pixel_buffer(handle: *const c_void) -> usize {
    run_host_extern_c(
        "host_gpu_lim_strong_count_pixel_buffer",
        || {
            if handle.is_null() {
                return 0;
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<PixelBufferRef>)`-shaped
            // (see `PixelBuffer::new`'s `from_arc_into_raw`). We
            // reconstruct the `Arc` temporarily, read the strong count, and
            // immediately re-leak it via `Arc::into_raw` so the strong count
            // returns to its pre-call value — `Arc::strong_count_from_raw`
            // is not part of the public stable API. The reconstruction runs
            // in HOST-COMPILED code regardless of caller DSO, so the cdylib
            // never has to know `PixelBufferRef`'s in-memory layout.
            unsafe {
                let arc =
                    Arc::from_raw(handle as *const crate::core::rhi::PixelBufferRef);
                let count = Arc::strong_count(&arc);
                let _ = Arc::into_raw(arc);
                count
            }
        },
        0,
    )
}

unsafe extern "C" fn host_gpu_lim_plane_base_address_pixel_buffer(
    handle: *const c_void,
    plane_index: u32,
) -> *mut u8 {
    run_host_extern_c(
        "host_gpu_lim_plane_base_address_pixel_buffer",
        || {
            if handle.is_null() {
                return core::ptr::null_mut();
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<PixelBufferRef>)`-shaped;
            // the leaked strong count keeps the `PixelBufferRef` alive for
            // the duration of the call. We borrow `&PixelBufferRef` rather
            // than reconstructing the Arc to avoid touching the refcount.
            unsafe {
                let pb_ref = &*(handle as *const crate::core::rhi::PixelBufferRef);
                pb_ref.plane_base_address(plane_index)
            }
        },
        core::ptr::null_mut(),
    )
}

unsafe extern "C" fn host_gpu_lim_plane_size_pixel_buffer(
    handle: *const c_void,
    plane_index: u32,
) -> u64 {
    run_host_extern_c(
        "host_gpu_lim_plane_size_pixel_buffer",
        || {
            if handle.is_null() {
                return 0;
            }
            // SAFETY: same as `host_gpu_lim_plane_base_address_pixel_buffer`.
            unsafe {
                let pb_ref = &*(handle as *const crate::core::rhi::PixelBufferRef);
                pb_ref.plane_size(plane_index)
            }
        },
        0,
    )
}

// -------------------------------------------------------------------------
// Texture Arc-handle lifecycle
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_clone_texture(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_texture",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is a `*const c_void` cast of
            // `Arc::into_raw(Arc<TextureInner>)` produced by host
            // code (see `Texture::from_arc_into_raw`).
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::rhi::texture::TextureInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_texture(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_texture",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with the `Arc::into_raw` in
            // `Texture::from_arc_into_raw` and any prior
            // `clone_texture` bumps.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::rhi::texture::TextureInner,
                );
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// Texture::native_handle DMA-BUF FD export (Phase F, #957)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_texture_native_dma_buf_fd(
    texture_handle: *const c_void,
) -> i64 {
    run_host_extern_c(
        "host_gpu_lim_texture_native_dma_buf_fd",
        || {
            if texture_handle.is_null() {
                return -1;
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `texture_handle` is the
                // `Arc::into_raw(Arc<TextureInner>)` pointer carried as the
                // cdylib-side `Texture::handle` field. Borrowing as
                // `&TextureInner` does not touch the refcount — the
                // caller's `Texture` keeps the Arc alive for the duration
                // of this dispatch.
                let inner = unsafe {
                    &*(texture_handle as *const crate::core::rhi::texture::TextureInner)
                };
                match inner.inner.export_dma_buf_fd() {
                    Ok(fd) => i64::from(fd),
                    Err(_) => -1,
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                // DMA-BUF is a Linux concept. macOS / Windows native
                // handles are deferred until those cdylib adapter paths
                // resume (see #908's AI Agent Notes).
                let _ = texture_handle;
                -1
            }
        },
        -1,
    )
}

// -------------------------------------------------------------------------
// Video-source timeline semaphore publish/clear (v12 — #958)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_set_video_source_timeline_semaphore(
    handle: *const c_void,
    timeline_handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_set_video_source_timeline_semaphore",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            if timeline_handle.is_null() {
                return;
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `timeline_handle` is a borrowed
                // `Arc::as_ptr(&Arc<HostVulkanTimelineSemaphore>)`
                // produced by the cdylib caller. Bump the refcount so
                // we can take a temporary owned Arc via `Arc::from_raw`;
                // the caller's Arc strong-count is unchanged.
                // Mirrors the `host_gpu_lim_register_texture` pattern
                // for borrowed `Arc<TextureInner>`-shaped handles.
                let ptr = timeline_handle
                    as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore;
                unsafe {
                    Arc::increment_strong_count(ptr);
                }
                let arc = unsafe { Arc::from_raw(ptr) };
                gpu.set_video_source_timeline_semaphore(&arc);
                // `arc` drops here, balancing the `increment_strong_count`
                // above. The slot holds its own `Arc::clone` (taken by
                // `set_video_source_timeline_semaphore` from the
                // borrow).
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = timeline_handle;
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clear_video_source_timeline_semaphore(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_clear_video_source_timeline_semaphore",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            #[cfg(target_os = "linux")]
            {
                gpu.clear_video_source_timeline_semaphore();
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = gpu;
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_wait_timeline_semaphore(
    _handle: *const c_void,
    timeline_handle: *const c_void,
    value: u64,
    timeout_ns: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_wait_timeline_semaphore",
        || {
            // `gpu_handle` is intentionally ignored — the timeline
            // borrow carries its own `vulkanalia::Device`, so the
            // wait runs against the timeline directly without
            // dereferencing any `GpuContext` instance. The handle
            // stays in the wire format for cross-slot consistency.
            if timeline_handle.is_null() {
                write_err(
                    "wait_timeline_semaphore: null timeline_handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `timeline_handle` is a borrowed pointer
                // from the cdylib's
                // `HostVulkanTimelineSemaphore::wait_via_vtable`
                // (which gets it via `self as *const Self`). The
                // host borrow lasts only for the duration of the
                // wait call. We call `wait_direct` to bypass the
                // `host_callbacks().is_some()` check on `wait()`
                // itself — otherwise the host would re-dispatch
                // through the vtable into infinite recursion.
                let timeline = unsafe {
                    &*(timeline_handle
                        as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore)
                };
                match timeline.wait_direct(value, timeout_ns) {
                    Ok(()) => 0,
                    Err(e) => {
                        write_err(
                            &format!("wait_timeline_semaphore: {e}"),
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        1
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (timeline_handle, value, timeout_ns);
                write_err(
                    "wait_timeline_semaphore: Linux-only",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                1
            }
        },
        1,
    )
}

// -------------------------------------------------------------------------
// host_video_source_timeline_arc — v14 (#1066)
// -------------------------------------------------------------------------

/// Clone the host's `Arc<HostVulkanTimelineSemaphore>` from the
/// publish slot and return the raw `Arc::into_raw` pointer to the
/// cdylib. The cdylib reconstitutes via `Arc::from_raw`; the host's
/// slot retains its own independent strong count. Returns null when
/// `gpu_handle` is null or when no producer has published a
/// timeline (the slot is `None`).
unsafe extern "C" fn host_gpu_lim_host_video_source_timeline_arc(
    handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_gpu_lim_host_video_source_timeline_arc",
        || -> *const c_void {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return std::ptr::null();
            };
            #[cfg(target_os = "linux")]
            {
                match gpu.video_source_timeline_semaphore() {
                    Some(arc) => Arc::into_raw(arc) as *const c_void,
                    None => std::ptr::null(),
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = gpu;
                std::ptr::null()
            }
        },
        std::ptr::null(),
    )
}

// -------------------------------------------------------------------------
// PooledTextureHandle lifecycle — drop-only (v4)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_drop_pooled_texture_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_pooled_texture_handle",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `Box::into_raw(Box<...>)` in
            // `PooledTextureHandle::from_parts`. Reclaiming via
            // `Box::from_raw` runs `Drop for PooledTextureHandleInner`
            // which releases the pool slot exactly once.
            unsafe {
                let _ = Box::from_raw(
                    handle as *mut crate::core::context::texture_pool::PooledTextureHandleInner,
                );
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// Method dispatch — Texture-related (v4)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_register_texture(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    texture_handle: *const c_void,
    initial_layout_raw: i32,
) {
    run_host_extern_c(
        "host_gpu_lim_register_texture",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            if texture_handle.is_null() {
                return;
            }
            // SAFETY: `texture_handle` is `Arc::into_raw(Arc<TextureInner>)`-shaped.
            // Bump the refcount so we can hand the cache its own owned
            // Arc; the caller's Texture continues to own its own.
            unsafe {
                Arc::increment_strong_count(
                    texture_handle as *const crate::core::rhi::texture::TextureInner,
                );
            }
            // SAFETY: same shape as above; from_raw + the bump above
            // gives us a fresh Arc with the right refcount.
            let texture_arc = unsafe {
                Arc::from_raw(
                    texture_handle as *const crate::core::rhi::texture::TextureInner,
                )
            };
            let inner_ref = &*texture_arc;
            let width = inner_ref.width();
            let height = inner_ref.height();
            let format = inner_ref.format();
            // Re-wrap into a Texture via the host's from_arc_into_raw
            // helper — leaks the Arc back into the texture cache shape.
            let texture =
                crate::core::rhi::texture::Texture::from_arc_into_raw(
                    texture_arc, width, height, format,
                );
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => return,
            };
            #[cfg(target_os = "linux")]
            {
                let layout = streamlib_consumer_rhi::VulkanLayout(initial_layout_raw);
                gpu.register_texture_with_layout(id_str, texture, layout);
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = initial_layout_raw;
                gpu.register_texture(id_str, texture);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_update_texture_registration_layout(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    layout_raw: i32,
) {
    run_host_extern_c(
        "host_gpu_lim_update_texture_registration_layout",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => return,
            };
            #[cfg(target_os = "linux")]
            {
                let layout = streamlib_consumer_rhi::VulkanLayout(layout_raw);
                gpu.update_texture_registration_layout(id_str, layout);
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (id_str, layout_raw);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_acquire_texture(
    handle: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    usage_bits: u32,
    out_pooled_handle: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_texture",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err("acquire_texture: null gpu handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if out_pooled_handle.is_null() {
                write_err("acquire_texture: null out_pooled_handle", err_buf, err_buf_cap, err_len);
                return 1;
            }
            let format = match format_raw {
                0 => streamlib_consumer_rhi::TextureFormat::Rgba8Unorm,
                1 => streamlib_consumer_rhi::TextureFormat::Rgba8UnormSrgb,
                2 => streamlib_consumer_rhi::TextureFormat::Bgra8Unorm,
                3 => streamlib_consumer_rhi::TextureFormat::Bgra8UnormSrgb,
                4 => streamlib_consumer_rhi::TextureFormat::Rgba16Float,
                5 => streamlib_consumer_rhi::TextureFormat::Rgba32Float,
                6 => streamlib_consumer_rhi::TextureFormat::Nv12,
                _ => {
                    let msg = format!("acquire_texture: invalid format_raw {}", format_raw);
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    return 1;
                }
            };
            let usage =
                streamlib_consumer_rhi::TextureUsages::from_bits_truncate(usage_bits);
            let desc = crate::core::context::TexturePoolDescriptor {
                width,
                height,
                format,
                usage,
                label: None,
            };
            match gpu.acquire_texture(&desc) {
                Ok(pooled) => {
                    // Move the host-built PooledTextureHandle into the
                    // caller's out-slot. The caller (cdylib) owns it
                    // after this — its Drop runs `drop_pooled_texture_handle`.
                    unsafe {
                        std::ptr::write(
                            out_pooled_handle
                                as *mut crate::core::context::PooledTextureHandle,
                            pooled,
                        );
                    }
                    0
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_lim_resolve_texture_by_surface_id(
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
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_resolve_texture_by_surface_id",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "resolve_texture_by_surface_id: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_texture.is_null() {
                write_err(
                    "resolve_texture_by_surface_id: null out_texture",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(surface_id_ptr, surface_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "resolve_texture_by_surface_id: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let texture_layout = if has_layout != 0 {
                Some(layout_raw)
            } else {
                None
            };
            match gpu.resolve_texture_by_surface_id(id_str, texture_layout, width, height) {
                Ok(texture) => {
                    // Hand the texture to the caller's out-slot. The
                    // caller (cdylib) owns it after this — its Drop
                    // runs `drop_texture`.
                    unsafe {
                        std::ptr::write(
                            out_texture as *mut crate::core::rhi::Texture,
                            texture,
                        );
                    }
                    0
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_lim_unregister_texture(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
) {
    run_host_extern_c(
        "host_gpu_lim_unregister_texture",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => return,
            };
            gpu.unregister_texture(id_str);
        },
        (),
    )
}

// -------------------------------------------------------------------------
// Escalate scope transition (Phase C3)
// -------------------------------------------------------------------------

/// Begin an escalate scope on the supplied `gpu_handle`. Mints a
/// unique opaque token via
/// [`crate::core::context::escalate_scope_registry::begin_escalate_scope`]
/// and writes it into `*out_scope_token`. Blocking on the gate is
/// expected — the host's escalate gate serializes against any
/// concurrent escalate scope on the same `GpuContext`.
unsafe extern "C" fn host_gpu_lim_escalate_begin(
    handle: *const c_void,
    out_scope_token: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_escalate_begin",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "escalate_begin: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1i32;
            };
            if out_scope_token.is_null() {
                write_err(
                    "escalate_begin: null out_scope_token",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1i32;
            }
            // begin_escalate_scope clones the Arc into the registry
            // and enters the gate; both operations succeed without
            // returning a fallible value.
            let token = crate::core::context::escalate_scope_registry::begin_escalate_scope(
                Arc::clone(gpu),
            );
            // SAFETY: out_scope_token is non-null per the check above.
            // Token encoding is just the u64 serial reinterpreted as
            // pointer-shaped; cdylib treats it as opaque.
            unsafe { *out_scope_token = token as *const c_void };
            0
        },
        1,
    )
}

/// End an escalate scope. Removes the bound `Arc<GpuContext>` from
/// the registry (releasing the escalate gate), then runs
/// [`GpuContext::wait_device_idle`] to match the host-mode escalate
/// path's scope-end semantics. Idempotent for stale or never-issued
/// tokens.
unsafe extern "C" fn host_gpu_lim_escalate_end(
    _handle: *const c_void,
    scope_token: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_escalate_end",
        || {
            let token = scope_token as u64;
            // Resolve the Arc BEFORE removing it from the registry so
            // we can call wait_device_idle. If the token is stale or
            // never-issued, this returns None — silently no-op (the
            // gate was never acquired by this token, so there's
            // nothing to release).
            let arc_clone = crate::core::context::escalate_scope_registry::with_scope(
                token,
                Arc::clone,
            );
            let removed = crate::core::context::escalate_scope_registry::end_escalate_scope(token);
            if !removed {
                return 0i32;
            }
            match arc_clone.as_ref().map(|arc| arc.wait_device_idle()) {
                Some(Ok(())) | None => 0,
                Some(Err(e)) => {
                    write_err(
                        &format!("escalate_end: wait_device_idle failed: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

// -------------------------------------------------------------------------
// Linux-only buffer Arc-handle lifecycle
// -------------------------------------------------------------------------
//
// All 4 buffer types (`StorageBuffer`, `UniformBuffer`, `VertexBuffer`,
// `IndexBuffer`) wrap `Arc<HostVulkanBuffer>` under the hood. The per-
// type callbacks are individually addressable in the vtable (so future
// per-type divergence doesn't force a re-version) but share the same
// host-side bookkeeping today. On non-Linux hosts the buffer types
// don't exist, so the callbacks compile to no-ops / error returns —
// the vtable slot is unconditional for ABI stability.

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_clone_host_vulkan_buffer_arc(handle: *const c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: `handle` is `Arc::into_raw(Arc<HostVulkanBuffer>)`-shaped
    // (see each buffer type's `from_arc_into_raw` constructor).
    unsafe {
        Arc::increment_strong_count(handle as *const crate::vulkan::rhi::HostVulkanBuffer);
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_drop_host_vulkan_buffer_arc(handle: *const c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: matched with the `Arc::into_raw` in each buffer type's
    // `from_arc_into_raw` constructor.
    unsafe {
        Arc::decrement_strong_count(handle as *const crate::vulkan::rhi::HostVulkanBuffer);
    }
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_clone_host_vulkan_buffer_arc(_handle: *const c_void) {
    // Buffer types only exist on Linux; this callback is unreachable
    // on other platforms. Defensive no-op.
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_drop_host_vulkan_buffer_arc(_handle: *const c_void) {
    // Buffer types only exist on Linux; defensive no-op.
}

// Per-type wrappers. Each just delegates to the shared
// `host_vulkan_buffer_arc` pair today but lives in the vtable as a
// dedicated slot, so a future per-type divergence (e.g. UniformBuffer
// growing a per-type cached field that needs its own clone semantics)
// only edits the wrapper without touching the vtable surface.

unsafe extern "C" fn host_gpu_lim_clone_storage_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_storage_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_storage_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_storage_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clone_uniform_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_uniform_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_uniform_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_uniform_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clone_vertex_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_vertex_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_vertex_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_vertex_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clone_index_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_index_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_index_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_index_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

// -------------------------------------------------------------------------
// Linux-only acquire_*_buffer method dispatch (v5)
// -------------------------------------------------------------------------

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_acquire_storage_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_storage_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_storage_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_storage_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_storage_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::StorageBuffer,
                            buf,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_acquire_uniform_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_uniform_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_uniform_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_uniform_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_uniform_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::UniformBuffer,
                            buf,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_acquire_vertex_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_vertex_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_vertex_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_vertex_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_vertex_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::VertexBuffer,
                            buf,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_acquire_index_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_index_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_index_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_index_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_index_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::IndexBuffer,
                            buf,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_acquire_storage_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_storage_buffer: StorageBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_acquire_uniform_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_uniform_buffer: UniformBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_acquire_vertex_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_vertex_buffer: VertexBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_acquire_index_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_index_buffer: IndexBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

// -------------------------------------------------------------------------
// TextureRegistration Arc-handle lifecycle
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_clone_texture_registration(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_texture_registration",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<TextureRegistrationInner>)`-shaped.
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::context::texture_registration::TextureRegistrationInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_texture_registration(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_texture_registration",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with the `Arc::into_raw` in
            // `TextureRegistration::from_arc_into_raw`.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::context::texture_registration::TextureRegistrationInner,
                );
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// TextureRegistration method dispatch (v6)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_texture_registration_texture(
    handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_gpu_lim_texture_registration_texture",
        || {
            if handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<TextureRegistrationInner>)`-shaped;
            // the Arc's strong count keeps the inner alive. We return
            // a pointer to the inner's `texture` field; the caller
            // (cdylib) deref's it as `*const Texture`. The pointer is
            // alive as long as the caller's `TextureRegistration` is.
            unsafe {
                let inner = &*(handle
                    as *const crate::core::context::texture_registration::TextureRegistrationInner);
                &inner.texture as *const crate::core::rhi::Texture as *const c_void
            }
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_gpu_lim_texture_registration_current_layout(
    handle: *const c_void,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_texture_registration_current_layout",
        || {
            if handle.is_null() {
                return 0; // VK_IMAGE_LAYOUT_UNDEFINED
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `handle` is `Arc::into_raw(...)`-shaped.
                unsafe {
                    let inner = &*(handle
                        as *const crate::core::context::texture_registration::TextureRegistrationInner);
                    inner
                        .current_layout
                        .load(std::sync::atomic::Ordering::Acquire)
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = handle;
                0
            }
        },
        0,
    )
}

unsafe extern "C" fn host_gpu_lim_texture_registration_update_layout(
    handle: *const c_void,
    layout_raw: i32,
) {
    run_host_extern_c(
        "host_gpu_lim_texture_registration_update_layout",
        || {
            if handle.is_null() {
                return;
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: same shape as
                // `host_gpu_lim_texture_registration_current_layout`.
                unsafe {
                    let inner = &*(handle
                        as *const crate::core::context::texture_registration::TextureRegistrationInner);
                    inner
                        .current_layout
                        .store(layout_raw, std::sync::atomic::Ordering::Release);
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (handle, layout_raw);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_resolve_texture_registration_by_surface_id(
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
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_resolve_texture_registration_by_surface_id",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "resolve_texture_registration_by_surface_id: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_registration.is_null() {
                write_err(
                    "resolve_texture_registration_by_surface_id: null out_registration",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(surface_id_ptr, surface_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "resolve_texture_registration_by_surface_id: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let texture_layout = if has_layout != 0 {
                Some(layout_raw)
            } else {
                None
            };
            match gpu.resolve_texture_registration_by_surface_id(id_str, texture_layout, width, height) {
                Ok(reg) => {
                    // SAFETY: out_registration points at caller-allocated
                    // stack storage for a `TextureRegistration` value.
                    unsafe {
                        std::ptr::write(
                            out_registration
                                as *mut crate::core::context::TextureRegistration,
                            reg,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// -------------------------------------------------------------------------
// RhiCommandQueue Arc-handle lifecycle + create_command_buffer
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_clone_rhi_command_queue(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_rhi_command_queue",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<RhiCommandQueueInner>)`-shaped.
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::rhi::command_queue::RhiCommandQueueInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_rhi_command_queue(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_rhi_command_queue",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with the `Arc::into_raw` in
            // `RhiCommandQueue::from_arc_into_raw`.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::rhi::command_queue::RhiCommandQueueInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_create_command_buffer_from_queue(
    queue_handle: *const c_void,
    out_cb: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_create_command_buffer_from_queue",
        || -> i32 {
            if queue_handle.is_null() {
                write_err(
                    "create_command_buffer_from_queue: null queue handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if out_cb.is_null() {
                write_err(
                    "create_command_buffer_from_queue: null out_cb",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: `queue_handle` is
            // `Arc::into_raw(Arc<RhiCommandQueueInner>)`-shaped; the
            // Arc's strong count keeps the inner alive for the duration.
            let inner = unsafe {
                &*(queue_handle
                    as *const crate::core::rhi::command_queue::RhiCommandQueueInner)
            };
            let result = inner.inner.create_command_buffer();
            match result {
                Ok(platform_cb) => {
                    let cb_inner =
                        crate::core::rhi::command_buffer::CommandBufferInner {
                            inner: platform_cb,
                        };
                    let cb = crate::core::rhi::CommandBuffer::from_inner(cb_inner);
                    // SAFETY: out_cb points at caller-allocated stack
                    // storage for a CommandBuffer value.
                    unsafe {
                        std::ptr::write(
                            out_cb as *mut crate::core::rhi::CommandBuffer,
                            cb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// -------------------------------------------------------------------------
// CommandBuffer lifecycle: drop + consume-semantics commits (v7)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_drop_command_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_command_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `Box::into_raw` in
            // `CommandBuffer::from_inner`.
            unsafe {
                let _ = Box::from_raw(
                    handle as *mut crate::core::rhi::command_buffer::CommandBufferInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_commit_command_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_commit_command_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `Box::into_raw` in
            // `CommandBuffer::from_inner`; the cdylib's commit(self)
            // nulls its local fields after this call so Drop won't
            // double-free. We move-out of the Box so the platform
            // commit can take ownership of the inner by-value.
            let cb_box = unsafe {
                Box::from_raw(
                    handle as *mut crate::core::rhi::command_buffer::CommandBufferInner,
                )
            };
            let cb_inner = *cb_box;
            cb_inner.inner.commit();
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_commit_and_wait_command_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_commit_and_wait_command_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: see `host_gpu_lim_commit_command_buffer`.
            let cb_box = unsafe {
                Box::from_raw(
                    handle as *mut crate::core::rhi::command_buffer::CommandBufferInner,
                )
            };
            let cb_inner = *cb_box;
            cb_inner.inner.commit_and_wait();
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_copy_texture_command_buffer(
    handle: *const c_void,
    src: *const c_void,
    dst: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_copy_texture_command_buffer",
        || {
            if handle.is_null() || src.is_null() || dst.is_null() {
                return;
            }
            // SAFETY: handle is `Box::into_raw(...)`-shaped; `&mut` is
            // sound because the cdylib's `&mut self` guarantees no
            // concurrent reference. src/dst are
            // `*const Texture` (layout locked by `texture_layout` test).
            unsafe {
                let cb_inner = &mut *(handle
                    as *mut crate::core::rhi::command_buffer::CommandBufferInner);
                let src_tex = &*(src as *const crate::core::rhi::Texture);
                let dst_tex = &*(dst as *const crate::core::rhi::Texture);
                // Re-use the existing platform-specific copy_texture
                // surface inside CommandBufferInner's `inner`.
                #[cfg(all(
                    not(feature = "backend-vulkan"),
                    any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
                ))]
                {
                    cb_inner.inner.copy_texture(
                        &src_tex.host_inner().inner,
                        &dst_tex.host_inner().inner,
                    );
                }
                #[cfg(any(
                    feature = "backend-vulkan",
                    all(target_os = "linux", not(feature = "backend-metal"))
                ))]
                {
                    use crate::host_rhi::HostTextureExt;
                    cb_inner
                        .inner
                        .copy_texture(src_tex.vulkan_inner(), dst_tex.vulkan_inner());
                }
                #[cfg(target_os = "windows")]
                {
                    cb_inner.inner.copy_texture(
                        &src_tex.host_inner().inner,
                        &dst_tex.host_inner().inner,
                    );
                }
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// GpuContextLimitedAccess command-queue / command-buffer / blit methods
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_command_queue(
    gpu_handle: *const c_void,
    out_queue: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_command_queue",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err("command_queue: null gpu handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if out_queue.is_null() {
                write_err("command_queue: null out_queue", err_buf, err_buf_cap, err_len);
                return 1;
            }
            // `gpu.command_queue()` returns `&RhiCommandQueue` (a borrow
            // from GpuContext's stored field). Clone into a fresh owned
            // β-shape for the caller — the Clone impl runs the host's
            // `clone_rhi_command_queue` callback (Arc refcount bump).
            let owned = gpu.command_queue().clone();
            // SAFETY: out_queue points at caller-allocated stack storage.
            unsafe {
                std::ptr::write(
                    out_queue as *mut crate::core::rhi::RhiCommandQueue,
                    owned,
                );
            }
            0
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_lim_create_command_buffer(
    gpu_handle: *const c_void,
    out_cb: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_create_command_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "create_command_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_cb.is_null() {
                write_err(
                    "create_command_buffer: null out_cb",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.create_command_buffer() {
                Ok(cb) => {
                    // SAFETY: out_cb points at caller-allocated storage.
                    unsafe {
                        std::ptr::write(
                            out_cb as *mut crate::core::rhi::CommandBuffer,
                            cb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_copy_pixel_buffer_to_texture(
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
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_copy_pixel_buffer_to_texture",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "copy_pixel_buffer_to_texture: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer.is_null() || texture.is_null() {
                write_err(
                    "copy_pixel_buffer_to_texture: null pixel_buffer or texture",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: pixel_buffer / texture point at β-shape values
            // whose layouts are locked by per-type regression tests.
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            let tex = unsafe { &*(texture as *const crate::core::rhi::Texture) };
            let id_bytes = unsafe { slice_from_raw(surface_id_ptr, surface_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "copy_pixel_buffer_to_texture: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match gpu.copy_pixel_buffer_to_texture(pb, tex, id_str, width, height) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_copy_pixel_buffer_to_texture(
    _gpu_handle: *const c_void,
    _pixel_buffer: *const c_void,
    _texture: *const c_void,
    _surface_id_ptr: *const u8,
    _surface_id_len: usize,
    _width: u32,
    _height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "copy_pixel_buffer_to_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

unsafe extern "C" fn host_gpu_lim_blit_copy(
    gpu_handle: *const c_void,
    src: *const c_void,
    dst: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_blit_copy",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err("blit_copy: null gpu handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if src.is_null() || dst.is_null() {
                write_err("blit_copy: null src or dst", err_buf, err_buf_cap, err_len);
                return 1;
            }
            // SAFETY: src / dst point at β-shape PixelBuffer values.
            let src_pb = unsafe { &*(src as *const crate::core::rhi::PixelBuffer) };
            let dst_pb = unsafe { &*(dst as *const crate::core::rhi::PixelBuffer) };
            match gpu.blit_copy(src_pb, dst_pb) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn host_gpu_lim_blit_copy_iosurface(
    gpu_handle: *const c_void,
    src_iosurface_ref: *const c_void,
    dst_pixel_buffer: *const c_void,
    width: u32,
    height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_blit_copy_iosurface",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "blit_copy_iosurface: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if dst_pixel_buffer.is_null() {
                write_err(
                    "blit_copy_iosurface: null dst_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let dst_pb = unsafe {
                &*(dst_pixel_buffer as *const crate::core::rhi::PixelBuffer)
            };
            let src_io = src_iosurface_ref as crate::apple::corevideo_ffi::IOSurfaceRef;
            match unsafe { gpu.blit_copy_iosurface(src_io, dst_pb, width, height) } {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn host_gpu_lim_blit_copy_iosurface(
    _gpu_handle: *const c_void,
    _src_iosurface_ref: *const c_void,
    _dst_pixel_buffer: *const c_void,
    _width: u32,
    _height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "blit_copy_iosurface: not available on this platform (macOS-only)",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

// -------------------------------------------------------------------------
// GpuContextLimitedAccessVTable — surface_store accessors
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_surface_store(
    gpu_handle: *const c_void,
    out_store: *mut c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_surface_store",
        || {
            // Always-clear: write a null-handle β-shape first so the
            // caller has a defined state even on error paths.
            if !out_store.is_null() {
                unsafe {
                    std::ptr::write(
                        out_store as *mut crate::core::context::SurfaceStore,
                        crate::core::context::SurfaceStore::null(),
                    );
                }
            }
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                return;
            };
            if out_store.is_null() {
                return;
            }
            // `gpu.surface_store()` returns `Option<SurfaceStore>` —
            // a fresh β-shape with Arc refcount already bumped when
            // Some. We write it into the out-param; the caller (cdylib
            // or host) takes ownership.
            if let Some(store) = gpu.surface_store() {
                unsafe {
                    std::ptr::write(
                        out_store as *mut crate::core::context::SurfaceStore,
                        store,
                    );
                }
            }
            // else: out_store already holds the null-handle β-shape.
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_check_out_surface(
    gpu_handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_check_out_surface",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "check_out_surface: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "check_out_surface: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "check_out_surface: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match gpu.check_out_surface(id_str) {
                Ok(pb) => {
                    unsafe {
                        std::ptr::write(
                            out_pixel_buffer as *mut crate::core::rhi::PixelBuffer,
                            pb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}


// -------------------------------------------------------------------------
// PixelBuffer acquire / get / resolve method-dispatch
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_acquire_pixel_buffer(
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
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_pixel_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "acquire_pixel_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "acquire_pixel_buffer: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let format = match pixel_format_from_raw(format_raw) {
                Some(f) => f,
                None => {
                    let msg = format!(
                        "acquire_pixel_buffer: invalid format_raw 0x{:08x}",
                        format_raw
                    );
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    return 1;
                }
            };
            match gpu.acquire_pixel_buffer(width, height, format) {
                Ok((pool_id, pb)) => {
                    write_id_bytes(
                        pool_id.as_str().as_bytes(),
                        out_pool_id_buf,
                        out_pool_id_cap,
                        out_pool_id_len,
                    );
                    unsafe {
                        std::ptr::write(
                            out_pixel_buffer as *mut crate::core::rhi::PixelBuffer,
                            pb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_lim_get_pixel_buffer(
    gpu_handle: *const c_void,
    pool_id_ptr: *const u8,
    pool_id_len: usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_get_pixel_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "get_pixel_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "get_pixel_buffer: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(pool_id_ptr, pool_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "get_pixel_buffer: pool_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let pool_id = crate::core::rhi::PixelBufferPoolId::from_str(id_str);
            match gpu.get_pixel_buffer(&pool_id) {
                Ok(pb) => {
                    unsafe {
                        std::ptr::write(
                            out_pixel_buffer as *mut crate::core::rhi::PixelBuffer,
                            pb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_lim_resolve_pixel_buffer_by_surface_id(
    gpu_handle: *const c_void,
    surface_id_ptr: *const u8,
    surface_id_len: usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_resolve_pixel_buffer_by_surface_id",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "resolve_pixel_buffer_by_surface_id: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "resolve_pixel_buffer_by_surface_id: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(surface_id_ptr, surface_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "resolve_pixel_buffer_by_surface_id: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match gpu.resolve_pixel_buffer_by_surface_id(id_str) {
                Ok(pb) => {
                    unsafe {
                        std::ptr::write(
                            out_pixel_buffer as *mut crate::core::rhi::PixelBuffer,
                            pb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}
/// Static [`GpuContextLimitedAccessVTable`] installed once per process.
/// Paired with the per-RuntimeContext gpu-limited handle returned by
/// [`HOST_RUNTIME_CONTEXT_VTABLE`]`::gpu_limited_access`.
// =============================================================================
// HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE — Phase C2
// =============================================================================
//
// FullAccess vtable bodies. Reached from cdylib code via the
// vtable-dispatched path of `GpuContextLimitedAccess::escalate`; the
// `gpu_handle` slot on every method is an opaque scope token issued
// by the LimitedAccess vtable's `escalate_begin` callback (Phase C3).
// Each body resolves the token to its bound `Arc<GpuContext>` via
// `with_full_scope_or_err`; missing tokens return
// `Error::InvalidEscalateScope`. The engine-internal in-process path
// constructs `GpuContextFullAccess` via `Self::new(GpuContext)` and
// reaches the same engine methods through `host_inner` rather than
// the vtable, so these callback bodies don't ever see an
// engine-internal `Box<Arc<GpuContext>>`-shaped handle.
//
// Kernel return handles: `*const VulkanComputeKernel` / etc., shaped
// as `Arc::into_raw(arc)`. Cdylib's `clone_*` / `drop_*` callbacks
// route refcount accounting through host-compiled code.

/// Defensive no-op. `GpuContextFullAccess::Drop` dispatches on the
/// struct's `handle_kind` discriminator directly without routing
/// through this vtable slot — host-mode (Boxed) runs `Box::from_raw`
/// in-process; cdylib-mode (ScopeToken) is a no-op (the cdylib's
/// escalate wrapper releases the gate via the LimitedAccess vtable's
/// `escalate_end` callback). The slot is preserved at the same vtable
/// offset for layout-version stability; calling it has no effect.
unsafe extern "C" fn host_gpu_full_drop_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_handle",
        || {
            let _ = handle;
        },
        (),
    )
}

// ---------------- Kernel Arc-handle lifecycle (Linux-only) ----------------

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_compute_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_compute_kernel",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: handle is `Arc::into_raw(Arc<VulkanComputeKernel>)`-shaped.
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanComputeKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_compute_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_compute_kernel",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: handle is `Arc::into_raw(Arc<VulkanComputeKernel>)`-shaped.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanComputeKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_graphics_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_graphics_kernel",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanGraphicsKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_graphics_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_graphics_kernel",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanGraphicsKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_ray_tracing_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_ray_tracing_kernel",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanRayTracingKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_ray_tracing_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_ray_tracing_kernel",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanRayTracingKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_texture_ring(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_texture_ring",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::context::TextureRingInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_texture_ring(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_texture_ring",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::context::TextureRingInner,
                );
            }
        },
        (),
    )
}

// β-shape v4 (#917) lifecycle callbacks. The handle is
// `Arc::into_raw(Arc<<Type>Inner>)`-shaped on the host side; cdylib
// code never sees the Inner layout, only the opaque handle paired
// with its β-shape vtable. Increment/decrement runs in host-compiled
// code where the Inner layout is known statically.

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_color_converter(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_color_converter",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::rhi::RhiColorConverterInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_color_converter(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_color_converter",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::rhi::RhiColorConverterInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_acceleration_structure(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_acceleration_structure",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle
                        as *const crate::vulkan::rhi::VulkanAccelerationStructureInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_acceleration_structure(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_acceleration_structure",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle
                        as *const crate::vulkan::rhi::VulkanAccelerationStructureInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_command_recorder(_handle: *const c_void) {
    // RhiCommandRecorder is Box-shaped (single-owner) — deliberately
    // NOT Clone per CommandBuffer precedent. This slot is reserved
    // infrastructure; the type-level absence of `Clone` for
    // `RhiCommandRecorder` ensures the host callback is never invoked
    // from typesafe code. If reached, it's a bug somewhere.
    run_host_extern_c(
        "host_gpu_full_clone_command_recorder",
        || {
            tracing::error!(
                "host_gpu_full_clone_command_recorder invoked — RhiCommandRecorder is \
                 not Clone-able (Box-shaped, single-owner). This is a bug."
            );
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_command_recorder(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_command_recorder",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: handle is `Box::into_raw(Box<RhiCommandRecorderInner>)`-shaped.
            // Reconstruct the Box and let Drop run.
            unsafe {
                let _ = Box::from_raw(
                    handle as *mut crate::vulkan::rhi::RhiCommandRecorderInner,
                );
            }
        },
        (),
    )
}

// Non-Linux stubs (callbacks must exist for the static layout, but
// the kernel types only ship on Linux).
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_compute_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_compute_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_graphics_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_graphics_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_ray_tracing_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_ray_tracing_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_texture_ring(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_texture_ring(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_color_converter(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_color_converter(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_acceleration_structure(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_acceleration_structure(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_command_recorder(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_command_recorder(_handle: *const c_void) {}

// ---------------- Kernel construction (Linux-only) ----------------

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_create_compute_kernel(
    scope_token: *const c_void,
    desc: *const ComputeKernelDescriptorRepr,
    out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_compute_kernel",
        || -> i32 {
            if desc.is_null() || out_kernel.is_null() {
                write_err(
                    "create_compute_kernel: null desc or out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let repr: &ComputeKernelDescriptorRepr = unsafe { &*desc };
            let result = with_full_scope_or_err(
                scope_token,
                "create_compute_kernel",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| unsafe {
                    crate::core::rhi::plugin_abi_bridge::with_decoded_compute_kernel_descriptor(
                        repr,
                        |rust_desc| gpu.create_compute_kernel(rust_desc),
                    )
                },
            );
            match result {
                Some(Ok(kernel)) => {
                    // `kernel` is the β-shape; its `handle` is the
                    // `Arc::into_raw(Arc<<Type>Inner>)` raw pointer
                    // already. Forget the β-shape so the strong ref
                    // transfers to cdylib; the cdylib reconstructs its
                    // own β-shape from { handle: raw, vtable } and
                    // never sees the `Arc<X>` internal layout.
                    let raw = kernel.handle;
                    std::mem::forget(kernel);
                    unsafe { std::ptr::write(out_kernel, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1, // err_buf populated by helper
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_create_graphics_kernel(
    scope_token: *const c_void,
    desc: *const GraphicsKernelDescriptorRepr,
    out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_graphics_kernel",
        || -> i32 {
            if desc.is_null() || out_kernel.is_null() {
                write_err(
                    "create_graphics_kernel: null desc or out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let repr: &GraphicsKernelDescriptorRepr = unsafe { &*desc };
            let result = with_full_scope_or_err(
                scope_token,
                "create_graphics_kernel",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| unsafe {
                    crate::core::rhi::plugin_abi_bridge::with_decoded_graphics_kernel_descriptor(
                        repr,
                        |rust_desc| gpu.create_graphics_kernel(rust_desc),
                    )
                },
            );
            match result {
                Some(Ok(kernel)) => {
                    // β-shape: extract the opaque handle (which is
                    // already `Arc::into_raw(Arc<<Type>Inner>)`-shaped)
                    // and `mem::forget` the wrapper so the strong ref
                    // transfers to cdylib. The cdylib reconstructs a
                    // fresh β-shape from { handle, vtable } and never
                    // sees the host's `Arc<X>` allocation header.
                    let raw = kernel.handle;
                    std::mem::forget(kernel);
                    unsafe { std::ptr::write(out_kernel, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_create_ray_tracing_kernel(
    gpu_handle: *const c_void,
    desc: *const RayTracingKernelDescriptorRepr,
    out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_ray_tracing_kernel",
        || -> i32 {
            if desc.is_null() || out_kernel.is_null() {
                write_err(
                    "create_ray_tracing_kernel: null desc or out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let repr: &RayTracingKernelDescriptorRepr = unsafe { &*desc };
            let result = with_full_scope_or_err(
                gpu_handle,
                "create_ray_tracing_kernel",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| unsafe {
                    crate::core::rhi::plugin_abi_bridge::with_decoded_ray_tracing_kernel_descriptor(
                        repr,
                        |rust_desc| gpu.create_ray_tracing_kernel(rust_desc),
                    )
                },
            );
            match result {
                Some(Ok(kernel)) => {
                    // β-shape: extract the opaque handle (which is
                    // already `Arc::into_raw(Arc<<Type>Inner>)`-shaped)
                    // and `mem::forget` the wrapper so the strong ref
                    // transfers to cdylib. The cdylib reconstructs a
                    // fresh β-shape from { handle, vtable } and never
                    // sees the host's `Arc<X>` allocation header.
                    let raw = kernel.handle;
                    std::mem::forget(kernel);
                    unsafe { std::ptr::write(out_kernel, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_create_texture_ring(
    scope_token: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    usage_bits: u32,
    count: usize,
    out_ring: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_texture_ring",
        || -> i32 {
            if out_ring.is_null() {
                write_err(
                    "create_texture_ring: null out_ring pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let format = match format_raw {
                0 => streamlib_consumer_rhi::TextureFormat::Rgba8Unorm,
                1 => streamlib_consumer_rhi::TextureFormat::Rgba8UnormSrgb,
                2 => streamlib_consumer_rhi::TextureFormat::Bgra8Unorm,
                3 => streamlib_consumer_rhi::TextureFormat::Bgra8UnormSrgb,
                4 => streamlib_consumer_rhi::TextureFormat::Rgba16Float,
                5 => streamlib_consumer_rhi::TextureFormat::Rgba32Float,
                6 => streamlib_consumer_rhi::TextureFormat::Nv12,
                _ => {
                    write_err(
                        &format!("create_texture_ring: invalid format_raw {format_raw}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let usages =
                streamlib_consumer_rhi::TextureUsages::from_bits_truncate(usage_bits);
            let result = with_full_scope_or_err(
                scope_token,
                "create_texture_ring",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_texture_ring(width, height, format, usages, count),
            );
            match result {
                Some(Ok(ring)) => {
                    // `ring` is the β-shape; its handle is
                    // `Arc::into_raw(Arc<TextureRingInner>)`-shaped.
                    let raw = ring.handle;
                    std::mem::forget(ring);
                    unsafe { std::ptr::write(out_ring, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

// Non-Linux stubs for the create_* callbacks.
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_create_compute_kernel(
    _gpu_handle: *const c_void,
    _desc: *const ComputeKernelDescriptorRepr,
    _out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_compute_kernel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_create_graphics_kernel(
    _gpu_handle: *const c_void,
    _desc: *const GraphicsKernelDescriptorRepr,
    _out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_graphics_kernel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_create_ray_tracing_kernel(
    _gpu_handle: *const c_void,
    _desc: *const RayTracingKernelDescriptorRepr,
    _out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_ray_tracing_kernel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_create_texture_ring(
    _gpu_handle: *const c_void,
    _width: u32,
    _height: u32,
    _format_raw: u32,
    _usage_bits: u32,
    _count: usize,
    _out_ring: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_texture_ring: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

// ---------------- Render-target allocation (Phase C3, Linux-only) ---------

/// Allocate a render-target-capable DMA-BUF-backed `VkImage`. Looks
/// up the bound `Arc<GpuContext>` via the scope_token; runs
/// [`crate::core::context::GpuContext::acquire_render_target_dma_buf_image`]
/// (which picks a tiled DRM modifier via the EGL probe and allocates
/// through the privileged RHI path), and writes the resulting
/// `Texture` β-shape into `*out_texture` on success.
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_acquire_render_target_dma_buf_image(
    scope_token: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    out_texture: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_acquire_render_target_dma_buf_image",
        || -> i32 {
            if out_texture.is_null() {
                write_err(
                    "acquire_render_target_dma_buf_image: null out_texture",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let format = match format_raw {
                0 => streamlib_consumer_rhi::TextureFormat::Rgba8Unorm,
                1 => streamlib_consumer_rhi::TextureFormat::Rgba8UnormSrgb,
                2 => streamlib_consumer_rhi::TextureFormat::Bgra8Unorm,
                3 => streamlib_consumer_rhi::TextureFormat::Bgra8UnormSrgb,
                4 => streamlib_consumer_rhi::TextureFormat::Rgba16Float,
                5 => streamlib_consumer_rhi::TextureFormat::Rgba32Float,
                6 => streamlib_consumer_rhi::TextureFormat::Nv12,
                _ => {
                    write_err(
                        &format!(
                            "acquire_render_target_dma_buf_image: invalid \
                             format_raw {}",
                            format_raw
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "acquire_render_target_dma_buf_image",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.acquire_render_target_dma_buf_image(width, height, format),
            );
            match result {
                Some(Ok(texture)) => {
                    unsafe {
                        std::ptr::write(
                            out_texture as *mut crate::core::rhi::Texture,
                            texture,
                        );
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1, // err_buf already populated by helper
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_acquire_render_target_dma_buf_image(
    _scope_token: *const c_void,
    _width: u32,
    _height: u32,
    _format_raw: u32,
    _out_texture: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_render_target_dma_buf_image: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

// ============================================================================
// Phase D (#906) — privileged-only FullAccess host callbacks.
// Each callback validates the `scope_token` via `with_full_scope_or_err`
// (resolving the bound `Arc<GpuContext>` from the escalate-scope registry)
// before dispatching to the resolved context.
// ============================================================================

unsafe extern "C" fn host_gpu_full_wait_device_idle(
    scope_token: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_wait_device_idle",
        || -> i32 {
            let result = with_full_scope_or_err(
                scope_token,
                "wait_device_idle",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.wait_device_idle(),
            );
            match result {
                Some(Ok(())) => 0,
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_full_acquire_output_texture(
    scope_token: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    out_id_buf: *mut u8,
    out_id_cap: usize,
    out_id_len: *mut usize,
    out_texture: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_acquire_output_texture",
        || -> i32 {
            if out_texture.is_null() || out_id_buf.is_null() || out_id_len.is_null() {
                write_err(
                    "acquire_output_texture: null out_texture / out_id_buf / out_id_len",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let format = match format_raw {
                0 => streamlib_consumer_rhi::TextureFormat::Rgba8Unorm,
                1 => streamlib_consumer_rhi::TextureFormat::Rgba8UnormSrgb,
                2 => streamlib_consumer_rhi::TextureFormat::Bgra8Unorm,
                3 => streamlib_consumer_rhi::TextureFormat::Bgra8UnormSrgb,
                4 => streamlib_consumer_rhi::TextureFormat::Rgba16Float,
                5 => streamlib_consumer_rhi::TextureFormat::Rgba32Float,
                6 => streamlib_consumer_rhi::TextureFormat::Nv12,
                _ => {
                    write_err(
                        &format!(
                            "acquire_output_texture: invalid format_raw {}",
                            format_raw
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "acquire_output_texture",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.acquire_output_texture(width, height, format),
            );
            match result {
                Some(Ok((id, texture))) => {
                    let id_bytes = id.as_bytes();
                    if id_bytes.len() > out_id_cap {
                        write_err(
                            "acquire_output_texture: surface id buffer too small",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        return 1;
                    }
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            id_bytes.as_ptr(),
                            out_id_buf,
                            id_bytes.len(),
                        );
                        std::ptr::write(out_id_len, id_bytes.len());
                        std::ptr::write(
                            out_texture as *mut crate::core::rhi::Texture,
                            texture,
                        );
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_upload_pixel_buffer_as_texture(
    scope_token: *const c_void,
    surface_id_ptr: *const u8,
    surface_id_len: usize,
    pixel_buffer: *const c_void,
    width: u32,
    height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_upload_pixel_buffer_as_texture",
        || -> i32 {
            if surface_id_ptr.is_null() || pixel_buffer.is_null() {
                write_err(
                    "upload_pixel_buffer_as_texture: null surface_id / pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_slice =
                unsafe { std::slice::from_raw_parts(surface_id_ptr, surface_id_len) };
            let surface_id = match std::str::from_utf8(id_slice) {
                Ok(s) => s,
                Err(e) => {
                    write_err(
                        &format!(
                            "upload_pixel_buffer_as_texture: surface_id not UTF-8: {e}"
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            // SAFETY: pixel_buffer is a borrowed `*const PixelBuffer`
            // pointer from the cdylib; valid for the duration of the call.
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            let result = with_full_scope_or_err(
                scope_token,
                "upload_pixel_buffer_as_texture",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.upload_pixel_buffer_as_texture(surface_id, pb, width, height),
            );
            match result {
                Some(Ok(())) => 0,
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_upload_pixel_buffer_as_texture(
    _scope_token: *const c_void,
    _surface_id_ptr: *const u8,
    _surface_id_len: usize,
    _pixel_buffer: *const c_void,
    _width: u32,
    _height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "upload_pixel_buffer_as_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_color_converter(
    scope_token: *const c_void,
    src_format_raw: u32,
    dst_format_raw: u32,
    out_converter: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_color_converter",
        || -> i32 {
            if out_converter.is_null() {
                write_err(
                    "color_converter: null out_converter",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let src = match pixel_format_from_raw(src_format_raw) {
                Some(f) => f,
                None => {
                    write_err(
                        &format!(
                            "color_converter: invalid src_format_raw {}",
                            src_format_raw
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let dst = match pixel_format_from_raw(dst_format_raw) {
                Some(f) => f,
                None => {
                    write_err(
                        &format!(
                            "color_converter: invalid dst_format_raw {}",
                            dst_format_raw
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "color_converter",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.color_converter(src, dst),
            );
            match result {
                Some(Ok(converter)) => {
                    // `converter` is the β-shape; its `handle` is the
                    // `Arc::into_raw(Arc<RhiColorConverterInner>)` pointer.
                    let raw = converter.handle;
                    std::mem::forget(converter);
                    unsafe { std::ptr::write(out_converter, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_color_converter(
    _scope_token: *const c_void,
    _src_format_raw: u32,
    _dst_format_raw: u32,
    _out_converter: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "color_converter: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_create_command_recorder(
    scope_token: *const c_void,
    label_ptr: *const u8,
    label_len: usize,
    out_recorder: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_command_recorder",
        || -> i32 {
            if out_recorder.is_null() || label_ptr.is_null() {
                write_err(
                    "create_command_recorder: null label_ptr / out_recorder",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let label_slice =
                unsafe { std::slice::from_raw_parts(label_ptr, label_len) };
            let label = match std::str::from_utf8(label_slice) {
                Ok(s) => s,
                Err(e) => {
                    write_err(
                        &format!("create_command_recorder: label not UTF-8: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "create_command_recorder",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_command_recorder(label),
            );
            match result {
                Some(Ok(recorder)) => {
                    // SAFETY: `recorder` is the β-shape — a
                    // `#[repr(C)] { handle: *const c_void, vtable: *const VTable }`
                    // 16-byte POD. Layout is byte-identical
                    // by `#[repr(C)]` invariant, not by rustc-version
                    // coupling. The cdylib reads the bits via
                    // `MaybeUninit::assume_init`; its `Drop` later
                    // dispatches through the vtable's
                    // `drop_command_recorder` slot which runs
                    // `Box::from_raw + drop` host-side.
                    unsafe {
                        std::ptr::write(
                            out_recorder as *mut crate::vulkan::rhi::RhiCommandRecorder,
                            recorder,
                        );
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_create_command_recorder(
    _scope_token: *const c_void,
    _label_ptr: *const u8,
    _label_len: usize,
    _out_recorder: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_command_recorder: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_build_triangles_blas(
    scope_token: *const c_void,
    label_ptr: *const u8,
    label_len: usize,
    vertices_ptr: *const f32,
    vertices_len: usize,
    indices_ptr: *const u32,
    indices_len: usize,
    out_blas: *mut *const c_void,
    out_device_address: *mut u64,
    out_storage_size: *mut u64,
    out_kind: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_build_triangles_blas",
        || -> i32 {
            if out_blas.is_null()
                || label_ptr.is_null()
                || out_device_address.is_null()
                || out_storage_size.is_null()
                || out_kind.is_null()
            {
                write_err(
                    "build_triangles_blas: null label_ptr / out-parameter pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let label_slice =
                unsafe { std::slice::from_raw_parts(label_ptr, label_len) };
            let label = match std::str::from_utf8(label_slice) {
                Ok(s) => s,
                Err(e) => {
                    write_err(
                        &format!("build_triangles_blas: label not UTF-8: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let vertices: &[f32] = if vertices_len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(vertices_ptr, vertices_len) }
            };
            let indices: &[u32] = if indices_len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(indices_ptr, indices_len) }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "build_triangles_blas",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.build_triangles_blas(label, vertices, indices),
            );
            match result {
                Some(Ok(blas)) => {
                    // `blas` is the β-shape — its `handle` is already
                    // `Arc::into_raw(Arc<VulkanAccelerationStructureInner>)`-shaped
                    // and its cached POD fields were populated by
                    // `VulkanAccelerationStructure::from_arc_into_raw`
                    // (host-mode mint path). Write them through the
                    // out-params so the cdylib's β-shape carries the
                    // real values instead of placeholder zeros. Forget
                    // the β-shape to keep the Arc strong count bumped;
                    // cdylib reconstructs its own β-shape from the
                    // handle + vtable + cached PODs.
                    let raw = blas.handle;
                    let device_address = blas.cached_device_address;
                    let storage_size = blas.cached_storage_size;
                    let kind = blas.cached_kind;
                    std::mem::forget(blas);
                    unsafe {
                        std::ptr::write(out_blas, raw);
                        std::ptr::write(out_device_address, device_address);
                        std::ptr::write(out_storage_size, storage_size);
                        std::ptr::write(out_kind, kind);
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_build_triangles_blas(
    _scope_token: *const c_void,
    _label_ptr: *const u8,
    _label_len: usize,
    _vertices_ptr: *const f32,
    _vertices_len: usize,
    _indices_ptr: *const u32,
    _indices_len: usize,
    _out_blas: *mut *const c_void,
    _out_device_address: *mut u64,
    _out_storage_size: *mut u64,
    _out_kind: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "build_triangles_blas: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_build_tlas(
    scope_token: *const c_void,
    label_ptr: *const u8,
    label_len: usize,
    instances_ptr: *const c_void,
    instances_len: usize,
    out_tlas: *mut *const c_void,
    out_device_address: *mut u64,
    out_storage_size: *mut u64,
    out_kind: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_build_tlas",
        || -> i32 {
            if out_tlas.is_null()
                || label_ptr.is_null()
                || out_device_address.is_null()
                || out_storage_size.is_null()
                || out_kind.is_null()
            {
                write_err(
                    "build_tlas: null label_ptr / out-parameter pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let label_slice =
                unsafe { std::slice::from_raw_parts(label_ptr, label_len) };
            let label = match std::str::from_utf8(label_slice) {
                Ok(s) => s,
                Err(e) => {
                    write_err(
                        &format!("build_tlas: label not UTF-8: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let instances: &[crate::vulkan::rhi::TlasInstanceDesc] = if instances_len
                == 0
            {
                &[]
            } else {
                // SAFETY: `instances_ptr` is `*const TlasInstanceDesc`
                // from the cdylib; layout is byte-identical under
                // rustc-version coupling. The slice is borrowed for
                // the call's duration.
                unsafe {
                    std::slice::from_raw_parts(
                        instances_ptr as *const crate::vulkan::rhi::TlasInstanceDesc,
                        instances_len,
                    )
                }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "build_tlas",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.build_tlas(label, instances),
            );
            match result {
                Some(Ok(tlas)) => {
                    // Same shape as `host_gpu_full_build_triangles_blas`:
                    // the β-shape's cached PODs are real (populated by
                    // `from_arc_into_raw` host-side); write them
                    // through the out-params so the cdylib's reassembled
                    // β-shape carries real values.
                    let raw = tlas.handle;
                    let device_address = tlas.cached_device_address;
                    let storage_size = tlas.cached_storage_size;
                    let kind = tlas.cached_kind;
                    std::mem::forget(tlas);
                    unsafe {
                        std::ptr::write(out_tlas, raw);
                        std::ptr::write(out_device_address, device_address);
                        std::ptr::write(out_storage_size, storage_size);
                        std::ptr::write(out_kind, kind);
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_build_tlas(
    _scope_token: *const c_void,
    _label_ptr: *const u8,
    _label_len: usize,
    _instances_ptr: *const c_void,
    _instances_len: usize,
    _out_tlas: *mut *const c_void,
    _out_device_address: *mut u64,
    _out_storage_size: *mut u64,
    _out_kind: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "build_tlas: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_supports_ray_tracing_pipeline(
    scope_token: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_supports_ray_tracing_pipeline",
        || -> i32 {
            let result = with_full_scope_or_err(
                scope_token,
                "supports_ray_tracing_pipeline",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| Ok::<bool, crate::core::Error>(gpu.supports_ray_tracing_pipeline()),
            );
            match result {
                Some(Ok(true)) => 1,
                Some(Ok(false)) => 0,
                Some(Err(_)) | None => -1,
            }
        },
        -1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_supports_ray_tracing_pipeline(
    _scope_token: *const c_void,
    _err_buf: *mut u8,
    _err_buf_cap: usize,
    _err_len: *mut usize,
) -> i32 {
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_gpu_capabilities(
    scope_token: *const c_void,
    out_caps: *mut streamlib_plugin_abi::GpuCapabilitiesRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_gpu_capabilities",
        || -> i32 {
            if out_caps.is_null() {
                write_err(
                    "gpu_capabilities: null out_caps pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let result = with_full_scope_or_err(
                scope_token,
                "gpu_capabilities",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| Ok::<_, crate::core::Error>(gpu.gpu_capabilities()),
            );
            match result {
                Some(Ok(snapshot)) => {
                    let mut repr = streamlib_plugin_abi::GpuCapabilitiesRepr {
                        device_name: [0u8; 256],
                        device_name_len: 0,
                        supports_external_memory: u8::from(
                            snapshot.supports_external_memory,
                        ),
                        supports_cross_device_dma_buf_probe: u8::from(
                            snapshot.supports_cross_device_dma_buf_probe,
                        ),
                        supports_ray_tracing_pipeline: u8::from(
                            snapshot.supports_ray_tracing_pipeline,
                        ),
                        _reserved_padding: 0,
                    };
                    let bytes = snapshot.device_name.as_bytes();
                    let n = bytes.len().min(repr.device_name.len());
                    repr.device_name[..n].copy_from_slice(&bytes[..n]);
                    repr.device_name_len = n as u32;
                    unsafe { std::ptr::write(out_caps, repr) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_gpu_capabilities(
    _scope_token: *const c_void,
    _out_caps: *mut streamlib_plugin_abi::GpuCapabilitiesRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "gpu_capabilities: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_create_timeline_semaphore(
    scope_token: *const c_void,
    initial_value: u64,
    out_handle: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_timeline_semaphore",
        || -> i32 {
            if out_handle.is_null() {
                write_err(
                    "create_timeline_semaphore: null out_handle pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let result = with_full_scope_or_err(
                scope_token,
                "create_timeline_semaphore",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_timeline_semaphore(initial_value),
            );
            match result {
                Some(Ok(arc)) => {
                    let raw = Arc::into_raw(arc) as *const c_void;
                    unsafe { std::ptr::write(out_handle, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_create_timeline_semaphore(
    _scope_token: *const c_void,
    _initial_value: u64,
    _out_handle: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_timeline_semaphore: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_import_dma_buf_storage_buffer(
    scope_token: *const c_void,
    fd: i32,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_import_dma_buf_storage_buffer",
        || -> i32 {
            if out_buffer.is_null() {
                write_err(
                    "import_dma_buf_storage_buffer: null out_buffer pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let result = with_full_scope_or_err(
                scope_token,
                "import_dma_buf_storage_buffer",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.import_dma_buf_storage_buffer(fd, byte_size),
            );
            match result {
                Some(Ok(buf)) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::StorageBuffer,
                            buf,
                        );
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_import_dma_buf_storage_buffer(
    _scope_token: *const c_void,
    _fd: i32,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "import_dma_buf_storage_buffer: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

unsafe extern "C" fn host_gpu_full_check_in_surface(
    scope_token: *const c_void,
    pixel_buffer: *const c_void,
    out_id_buf: *mut u8,
    out_id_cap: usize,
    out_id_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_check_in_surface",
        || -> i32 {
            if pixel_buffer.is_null() || out_id_buf.is_null() || out_id_len.is_null() {
                write_err(
                    "check_in_surface: null pixel_buffer / out_id_buf / out_id_len",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: pixel_buffer is borrowed from the cdylib for the
            // duration of the call.
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            let result = with_full_scope_or_err(
                scope_token,
                "check_in_surface",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.check_in_surface(pb),
            );
            match result {
                Some(Ok(id)) => {
                    let id_bytes = id.as_bytes();
                    if id_bytes.len() > out_id_cap {
                        write_err(
                            "check_in_surface: surface id buffer too small",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        return 1;
                    }
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            id_bytes.as_ptr(),
                            out_id_buf,
                            id_bytes.len(),
                        );
                        std::ptr::write(out_id_len, id_bytes.len());
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

/// Clone the host's `Arc<HostVulkanDevice>` and return the raw
/// `Arc::into_raw` pointer. Used by in-process workspace plugin cdylibs
/// (#1004 dlopen smoke fixtures for the surface adapters) that need to
/// construct a host-flavor `XxxSurfaceAdapter<HostVulkanDevice>` to
/// exercise `acquire_write` → `view_mut` → release through the cdylib
/// boundary. On null/stale token returns a null pointer.
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_host_vulkan_device_arc(
    scope_token: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_gpu_full_host_vulkan_device_arc",
        || -> *const c_void {
            let token = scope_token as u64;
            crate::core::context::escalate_scope_registry::with_scope(token, |gpu| {
                let device = gpu.device();
                let host_device =
                    crate::host_rhi::HostGpuDeviceExt::vulkan_device(device.as_ref());
                let arc = Arc::clone(host_device);
                Arc::into_raw(arc) as *const c_void
            })
            .unwrap_or(std::ptr::null())
        },
        std::ptr::null(),
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_host_vulkan_device_arc(
    _scope_token: *const c_void,
) -> *const c_void {
    std::ptr::null()
}

/// Clone the host's `Arc<HostVulkanTexture>` backing a `Texture`
/// β-shape and return the raw `Arc::into_raw` pointer. Second bridge
/// of the cdylib-side adapter-construction chain: cdylibs can't
/// reach `Texture::host_inner()` (panics in cdylib mode), so they
/// dispatch through this slot to obtain a real
/// `Arc<HostVulkanTexture>` for calls like
/// `OpenGlSurfaceAdapter::register_host_surface`. On null
/// `texture_handle` returns a null pointer.
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_host_vulkan_texture_arc(
    texture_handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_gpu_full_host_vulkan_texture_arc",
        || -> *const c_void {
            if texture_handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: `texture_handle` is the same opaque
            // `Arc::into_raw(Arc<TextureInner>)` pointer cached on the
            // `Texture` β-shape's `handle` field (see
            // `Texture::from_arc_into_raw`). The leaked strong count
            // keeps the `TextureInner` alive at least until the
            // β-shape's `Drop` runs. We borrow without taking
            // ownership, clone the inner `Arc<HostVulkanTexture>`, and
            // return its raw pointer with the strong count bumped by 1.
            let inner = unsafe {
                &*(texture_handle
                    as *const crate::core::rhi::texture::TextureInner)
            };
            let arc = Arc::clone(&inner.inner);
            Arc::into_raw(arc) as *const c_void
        },
        std::ptr::null(),
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_host_vulkan_texture_arc(
    _texture_handle: *const c_void,
) -> *const c_void {
    std::ptr::null()
}

pub static HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE: GpuContextFullAccessVTable =
    GpuContextFullAccessVTable {
        layout_version: GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        drop_handle: host_gpu_full_drop_handle,
        clone_compute_kernel: host_gpu_full_clone_compute_kernel,
        drop_compute_kernel: host_gpu_full_drop_compute_kernel,
        clone_graphics_kernel: host_gpu_full_clone_graphics_kernel,
        drop_graphics_kernel: host_gpu_full_drop_graphics_kernel,
        clone_ray_tracing_kernel: host_gpu_full_clone_ray_tracing_kernel,
        drop_ray_tracing_kernel: host_gpu_full_drop_ray_tracing_kernel,
        clone_texture_ring: host_gpu_full_clone_texture_ring,
        drop_texture_ring: host_gpu_full_drop_texture_ring,
        // v4 β-shape lifecycle slots (#917).
        clone_color_converter: host_gpu_full_clone_color_converter,
        drop_color_converter: host_gpu_full_drop_color_converter,
        clone_acceleration_structure: host_gpu_full_clone_acceleration_structure,
        drop_acceleration_structure: host_gpu_full_drop_acceleration_structure,
        clone_command_recorder: host_gpu_full_clone_command_recorder,
        drop_command_recorder: host_gpu_full_drop_command_recorder,
        create_compute_kernel: host_gpu_full_create_compute_kernel,
        create_graphics_kernel: host_gpu_full_create_graphics_kernel,
        create_ray_tracing_kernel: host_gpu_full_create_ray_tracing_kernel,
        create_texture_ring: host_gpu_full_create_texture_ring,
        acquire_render_target_dma_buf_image:
            host_gpu_full_acquire_render_target_dma_buf_image,
        // Phase D (#906) entries.
        wait_device_idle: host_gpu_full_wait_device_idle,
        acquire_output_texture: host_gpu_full_acquire_output_texture,
        upload_pixel_buffer_as_texture: host_gpu_full_upload_pixel_buffer_as_texture,
        color_converter: host_gpu_full_color_converter,
        create_command_recorder: host_gpu_full_create_command_recorder,
        build_triangles_blas: host_gpu_full_build_triangles_blas,
        build_tlas: host_gpu_full_build_tlas,
        supports_ray_tracing_pipeline: host_gpu_full_supports_ray_tracing_pipeline,
        check_in_surface: host_gpu_full_check_in_surface,
        gpu_capabilities: host_gpu_full_gpu_capabilities,
        create_timeline_semaphore: host_gpu_full_create_timeline_semaphore,
        import_dma_buf_storage_buffer: host_gpu_full_import_dma_buf_storage_buffer,
        host_vulkan_device_arc: host_gpu_full_host_vulkan_device_arc,
        host_vulkan_texture_arc: host_gpu_full_host_vulkan_texture_arc,
    };

/// Pointer to the [`GpuContextFullAccessVTable`] this DSO should
/// dispatch through. Same DSO-routing rule as
/// [`host_gpu_context_limited_access_vtable`]: host mode resolves to
/// the local `&HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE` static, cdylib
/// mode resolves to the host-installed pointer cached on
/// [`HostServices::gpu_context_full_access_vtable`].
pub fn host_gpu_context_full_access_vtable() -> *const GpuContextFullAccessVTable {
    match host_callbacks() {
        Some(c) if !c.gpu_context_full_access_vtable.is_null() => {
            c.gpu_context_full_access_vtable
        }
        _ => &HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE,
    }
}

pub static HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE: GpuContextLimitedAccessVTable =
    GpuContextLimitedAccessVTable {
        layout_version: GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        clone_handle: host_gpu_lim_clone_handle,
        drop_handle: host_gpu_lim_drop_handle,
        clone_pixel_buffer: host_gpu_lim_clone_pixel_buffer,
        drop_pixel_buffer: host_gpu_lim_drop_pixel_buffer,
        strong_count_pixel_buffer: host_gpu_lim_strong_count_pixel_buffer,
        plane_base_address_pixel_buffer: host_gpu_lim_plane_base_address_pixel_buffer,
        plane_size_pixel_buffer: host_gpu_lim_plane_size_pixel_buffer,
        clone_texture: host_gpu_lim_clone_texture,
        drop_texture: host_gpu_lim_drop_texture,
        drop_pooled_texture_handle: host_gpu_lim_drop_pooled_texture_handle,
        register_texture: host_gpu_lim_register_texture,
        update_texture_registration_layout: host_gpu_lim_update_texture_registration_layout,
        acquire_texture: host_gpu_lim_acquire_texture,
        resolve_texture_by_surface_id: host_gpu_lim_resolve_texture_by_surface_id,
        unregister_texture: host_gpu_lim_unregister_texture,
        clone_storage_buffer: host_gpu_lim_clone_storage_buffer,
        drop_storage_buffer: host_gpu_lim_drop_storage_buffer,
        clone_uniform_buffer: host_gpu_lim_clone_uniform_buffer,
        drop_uniform_buffer: host_gpu_lim_drop_uniform_buffer,
        clone_vertex_buffer: host_gpu_lim_clone_vertex_buffer,
        drop_vertex_buffer: host_gpu_lim_drop_vertex_buffer,
        clone_index_buffer: host_gpu_lim_clone_index_buffer,
        drop_index_buffer: host_gpu_lim_drop_index_buffer,
        acquire_storage_buffer: host_gpu_lim_acquire_storage_buffer,
        acquire_uniform_buffer: host_gpu_lim_acquire_uniform_buffer,
        acquire_vertex_buffer: host_gpu_lim_acquire_vertex_buffer,
        acquire_index_buffer: host_gpu_lim_acquire_index_buffer,
        clone_texture_registration: host_gpu_lim_clone_texture_registration,
        drop_texture_registration: host_gpu_lim_drop_texture_registration,
        texture_registration_texture: host_gpu_lim_texture_registration_texture,
        texture_registration_current_layout: host_gpu_lim_texture_registration_current_layout,
        texture_registration_update_layout: host_gpu_lim_texture_registration_update_layout,
        resolve_texture_registration_by_surface_id:
            host_gpu_lim_resolve_texture_registration_by_surface_id,
        clone_rhi_command_queue: host_gpu_lim_clone_rhi_command_queue,
        drop_rhi_command_queue: host_gpu_lim_drop_rhi_command_queue,
        create_command_buffer_from_queue: host_gpu_lim_create_command_buffer_from_queue,
        drop_command_buffer: host_gpu_lim_drop_command_buffer,
        commit_command_buffer: host_gpu_lim_commit_command_buffer,
        commit_and_wait_command_buffer: host_gpu_lim_commit_and_wait_command_buffer,
        copy_texture_command_buffer: host_gpu_lim_copy_texture_command_buffer,
        command_queue: host_gpu_lim_command_queue,
        create_command_buffer: host_gpu_lim_create_command_buffer,
        copy_pixel_buffer_to_texture: host_gpu_lim_copy_pixel_buffer_to_texture,
        blit_copy: host_gpu_lim_blit_copy,
        blit_copy_iosurface: host_gpu_lim_blit_copy_iosurface,
        surface_store: host_gpu_lim_surface_store,
        check_out_surface: host_gpu_lim_check_out_surface,
        acquire_pixel_buffer: host_gpu_lim_acquire_pixel_buffer,
        get_pixel_buffer: host_gpu_lim_get_pixel_buffer,
        resolve_pixel_buffer_by_surface_id: host_gpu_lim_resolve_pixel_buffer_by_surface_id,
        escalate_begin: host_gpu_lim_escalate_begin,
        escalate_end: host_gpu_lim_escalate_end,
        texture_native_dma_buf_fd: host_gpu_lim_texture_native_dma_buf_fd,
        set_video_source_timeline_semaphore:
            host_gpu_lim_set_video_source_timeline_semaphore,
        clear_video_source_timeline_semaphore:
            host_gpu_lim_clear_video_source_timeline_semaphore,
        wait_timeline_semaphore: host_gpu_lim_wait_timeline_semaphore,
        host_video_source_timeline_arc:
            host_gpu_lim_host_video_source_timeline_arc,
    };

/// Pointer to the [`GpuContextLimitedAccessVTable`] this DSO should
/// dispatch through. Same DSO-routing rule as
/// [`host_runtime_context_vtable`].
pub fn host_gpu_context_limited_access_vtable() -> *const GpuContextLimitedAccessVTable {
    match host_callbacks() {
        Some(c) if !c.gpu_context_limited_access_vtable.is_null() => {
            c.gpu_context_limited_access_vtable
        }
        _ => &HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE,
    }
}
#[cfg(test)]
mod gpu_full_access_vtable_tests {
    use super::*;
    use streamlib_plugin_abi::{
        ComputeKernelDescriptorRepr, GraphicsKernelDescriptorRepr,
        RayTracingKernelDescriptorRepr,
    };

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn drop_handle_handles_null_no_crash() {
        // Null handle is documented as a no-op; this just exercises
        // the early-return guard.
        unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.drop_handle)(std::ptr::null());
        }
    }

    #[test]
    fn create_compute_kernel_returns_error_on_null_scope_token() {
        // Post-C3: gpu_handle is interpreted as a scope_token; a null
        // pointer corresponds to scope_token = 0, which is reserved as
        // "never issued" — `with_scope` returns None and the callback
        // returns an "invalid escalate scope" error.
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let bindings_buf: [streamlib_plugin_abi::ComputeBindingSpecRepr; 0] = [];
        let repr = ComputeKernelDescriptorRepr {
            label_ptr: "test".as_ptr(),
            label_len: 4,
            spv_ptr: std::ptr::null(),
            spv_len: 0,
            bindings_ptr: bindings_buf.as_ptr(),
            bindings_len: 0,
            push_constant_size: 0,
            _reserved_padding: 0,
        };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_compute_kernel)(
                std::ptr::null(),
                &repr,
                &mut out,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_compute_kernel: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null(), "out_kernel must not be written on error");
    }

    #[test]
    fn create_graphics_kernel_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let repr: GraphicsKernelDescriptorRepr = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_graphics_kernel)(
                std::ptr::null(),
                &repr,
                &mut out,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_graphics_kernel: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null());
    }

    #[test]
    fn create_ray_tracing_kernel_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let repr: RayTracingKernelDescriptorRepr = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_ray_tracing_kernel)(
                std::ptr::null(),
                &repr,
                &mut out,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_ray_tracing_kernel: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null());
    }

    #[test]
    fn create_texture_ring_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_texture_ring)(
                std::ptr::null(),
                64,
                64,
                0, // Rgba8Unorm
                0, // no usage bits
                2,
                &mut out,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_texture_ring: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null());
    }

    #[test]
    fn acquire_render_target_dma_buf_image_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE
                .acquire_render_target_dma_buf_image)(
                std::ptr::null(),
                64,
                64,
                0, // Rgba8Unorm
                &mut out as *mut _ as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "acquire_render_target_dma_buf_image: invalid escalate scope"
            ),
            "got: {msg}"
        );
    }

    #[test]
    fn acquire_render_target_dma_buf_image_returns_error_on_invalid_format() {
        // Even with an invalid format, the null scope-token check would
        // run after the format decode — so feeding a token of 0 (which
        // would later fail scope lookup) but an invalid format ensures
        // the format-validation path fires.
        let (mut buf, mut len) = make_err_buf();
        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE
                .acquire_render_target_dma_buf_image)(
                std::ptr::null(),
                64,
                64,
                99, // invalid format_raw
                &mut out as *mut _ as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "acquire_render_target_dma_buf_image: invalid format_raw"
            ),
            "got: {msg}"
        );
    }

    // ============================================================================
    // Phase D (#906) — tier-1 wire-format tests for the 9 new FullAccess slots
    // ============================================================================

    #[test]
    fn wait_device_idle_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.wait_device_idle)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("wait_device_idle: invalid escalate scope"),
            "got: {msg}"
        );
    }

    #[test]
    fn acquire_output_texture_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.acquire_output_texture)(
                std::ptr::null(),
                64,
                64,
                0,
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                &mut out as *mut _ as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_output_texture: invalid escalate scope"),
            "got: {msg}"
        );
    }

    #[test]
    fn acquire_output_texture_returns_error_on_invalid_format() {
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.acquire_output_texture)(
                std::ptr::null(),
                64,
                64,
                99,
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                &mut out as *mut _ as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_output_texture: invalid format_raw"),
            "got: {msg}"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn upload_pixel_buffer_as_texture_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        // We pass non-null surface_id + a "borrowed" PixelBuffer placeholder
        // through the null-pointer guard; the scope-token check then fires
        // because the token is null/zero.
        let sid = b"abc";
        let pb: crate::core::rhi::PixelBuffer = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.upload_pixel_buffer_as_texture)(
                std::ptr::null(),
                sid.as_ptr(),
                sid.len(),
                &pb as *const _ as *const c_void,
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        // Leak the zeroed PixelBuffer to avoid running its (cdylib-mode)
        // Drop on a null handle — that would dispatch through a null
        // vtable. The null-handle Drop guard short-circuits, but
        // mem::forget makes the intent explicit.
        std::mem::forget(pb);
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("upload_pixel_buffer_as_texture: invalid escalate scope"),
            "got: {msg}"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn color_converter_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.color_converter)(
                std::ptr::null(),
                0, // src
                0, // dst
                &mut out,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("color_converter: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn create_command_recorder_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let label = b"test_recorder";
        let mut out: std::mem::MaybeUninit<crate::vulkan::rhi::RhiCommandRecorder> =
            std::mem::MaybeUninit::uninit();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_command_recorder)(
                std::ptr::null(),
                label.as_ptr(),
                label.len(),
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_command_recorder: invalid escalate scope"),
            "got: {msg}"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn build_triangles_blas_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let label = b"test_blas";
        let vertices = [0.0f32, 0.0, 0.0];
        let indices = [0u32, 1, 2];
        let mut out: *const c_void = std::ptr::null();
        let mut out_device_address: u64 = 0;
        let mut out_storage_size: u64 = 0;
        let mut out_kind: u32 = 0;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.build_triangles_blas)(
                std::ptr::null(),
                label.as_ptr(),
                label.len(),
                vertices.as_ptr(),
                vertices.len(),
                indices.as_ptr(),
                indices.len(),
                &mut out,
                &mut out_device_address as *mut u64,
                &mut out_storage_size as *mut u64,
                &mut out_kind as *mut u32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("build_triangles_blas: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null());
        // Out-params untouched on failure.
        assert_eq!(out_device_address, 0);
        assert_eq!(out_storage_size, 0);
        assert_eq!(out_kind, 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn build_tlas_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let label = b"test_tlas";
        let mut out: *const c_void = std::ptr::null();
        let mut out_device_address: u64 = 0;
        let mut out_storage_size: u64 = 0;
        let mut out_kind: u32 = 0;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.build_tlas)(
                std::ptr::null(),
                label.as_ptr(),
                label.len(),
                std::ptr::null(),
                0,
                &mut out,
                &mut out_device_address as *mut u64,
                &mut out_storage_size as *mut u64,
                &mut out_kind as *mut u32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("build_tlas: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null());
        // Out-params untouched on failure.
        assert_eq!(out_device_address, 0);
        assert_eq!(out_storage_size, 0);
        assert_eq!(out_kind, 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn supports_ray_tracing_pipeline_returns_negative_one_on_null_scope_token() {
        // Returns -1 for "invalid scope token" (since 1/0 are valid yes/no
        // bool returns). The error message goes to err_buf.
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.supports_ray_tracing_pipeline)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, -1, "null scope token must return -1, got {rc}");
    }

    #[test]
    fn check_in_surface_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let pb: crate::core::rhi::PixelBuffer = unsafe { std::mem::zeroed() };
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.check_in_surface)(
                std::ptr::null(),
                &pb as *const _ as *const c_void,
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        std::mem::forget(pb);
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("check_in_surface: invalid escalate scope"),
            "got: {msg}"
        );
    }

    #[test]
    fn vtable_layout_version_matches_constant() {
        assert_eq!(
            HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.layout_version,
            streamlib_plugin_abi::GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION
        );
    }

    // v9 slot: host_vulkan_device_arc takes a scope token (not an
    // err-buf). The null-token case bottoms out in
    // `with_scope(0, ...) → None`, so the callback returns a null
    // pointer. Mental-revert: stub the callback body to call
    // `Arc::into_raw(...)` directly on a freshly-cloned Arc without
    // checking the token; this test trips on the resulting non-null
    // return and the unmatched `from_raw` would Drop the Arc, lowering
    // the refcount on the host's actual `Arc<HostVulkanDevice>`.
    #[test]
    fn host_vulkan_device_arc_returns_null_on_null_token() {
        let raw = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.host_vulkan_device_arc)(
                std::ptr::null(),
            )
        };
        assert!(raw.is_null(), "null scope token must yield null pointer");
    }

    // v10 slot: host_vulkan_texture_arc takes a raw texture handle
    // (not an err-buf). The null-handle case short-circuits in the
    // wrapper before any deref, returning a null pointer. Mental-
    // revert: remove the `if texture_handle.is_null()` guard inside
    // `host_gpu_full_host_vulkan_texture_arc`; the wrapper would then
    // UB-deref the null pointer as `*const TextureInner` and the test
    // runner would SIGSEGV.
    #[test]
    fn host_vulkan_texture_arc_returns_null_on_null_handle() {
        let raw = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.host_vulkan_texture_arc)(
                std::ptr::null(),
            )
        };
        assert!(raw.is_null(), "null texture handle must yield null pointer");
    }

    #[test]
    fn host_services_for_self_wires_full_access_vtable() {
        let node = match crate::iceoryx2::Iceoryx2Node::new() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    target: "streamlib::tests::gpu_full_access_vtable",
                    error = %e,
                    "skipping host_services_for_self wiring assertion: iceoryx2 init unavailable in this env"
                );
                return;
            }
        };
        let services = super::super::runtime_facing::host_services_for_self(&node);
        assert!(
            !services.gpu_context_full_access_vtable.is_null(),
            "host should wire the FullAccess vtable pointer"
        );
        let installed_version =
            unsafe { (*services.gpu_context_full_access_vtable).layout_version };
        assert_eq!(
            installed_version,
            streamlib_plugin_abi::GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION
        );
    }
}
#[cfg(test)]
mod gpu_lim_escalate_vtable_tests {
    //! Tier-1 wire-format + round-trip tests for C3's escalate_begin
    //! and escalate_end vtable entries.
    //!
    //! Tests that construct a real `GpuContext` carry `#[serial]` to
    //! prevent the NVIDIA Linux dual-`VkDevice` SIGSEGV
    //! (`docs/learnings/nvidia-dual-vulkan-device-crash.md`) when run
    //! against other VkDevice-creating tests in the workspace lib
    //! suite.

    use super::*;
    use serial_test::serial;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    /// Build a host-mode gpu_handle (the `Box<Arc<GpuContext>>`-shaped
    /// pointer that `GpuContextLimitedAccess::new` produces) so the
    /// `escalate_begin` callback can run end-to-end against a real
    /// `Arc<GpuContext>`. Skips when no GPU device is available.
    fn make_host_handle() -> Option<(*const c_void, Arc<crate::core::context::GpuContext>)> {
        let gpu = crate::core::context::GpuContext::init_for_platform().ok()?;
        let arc = Arc::new(gpu);
        let boxed: Box<Arc<crate::core::context::GpuContext>> = Box::new(Arc::clone(&arc));
        let handle = Box::into_raw(boxed) as *const c_void;
        Some((handle, arc))
    }

    /// Free a host_handle minted by `make_host_handle` — pairs with
    /// the `Box::into_raw`.
    unsafe fn free_host_handle(handle: *const c_void) {
        let _ = unsafe {
            Box::from_raw(handle as *mut Arc<crate::core::context::GpuContext>)
        };
    }

    #[test]
    fn escalate_begin_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut token: *const c_void = std::ptr::null();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                std::ptr::null(),
                &mut token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("escalate_begin: null gpu handle"), "got: {msg}");
        assert!(token.is_null(), "scope token must not be written on error");
    }

    #[test]
    #[serial]
    fn escalate_begin_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            tracing::warn!(
                target: "streamlib::tests::escalate_vtable",
                "skipping escalate_begin null-out test: no GPU device"
            );
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("escalate_begin: null out_scope_token"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    fn escalate_end_is_idempotent_for_stale_token() {
        // escalate_end with a never-issued token is a clean no-op
        // (returns 0; doesn't release any gate). Documented as
        // idempotent in the registry.
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                std::ptr::null(),
                u64::MAX as *const c_void, // never-issued token
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 0);
        assert_eq!(len, 0, "no error message expected for stale token");
    }

    #[test]
    #[serial]
    fn round_trip_begin_then_end_releases_gate() {
        let Some((handle, _arc)) = make_host_handle() else {
            tracing::warn!(
                target: "streamlib::tests::escalate_vtable",
                "skipping round-trip test: no GPU device"
            );
            return;
        };

        let (mut buf, mut len) = make_err_buf();
        let mut token: *const c_void = std::ptr::null();
        let begin_rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                &mut token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(begin_rc, 0);
        assert!(!token.is_null(), "scope token must be written on success");

        let end_rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                handle,
                token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(end_rc, 0);

        // Begin again on the same handle — gate must have been
        // released, so this succeeds without blocking. (If the gate
        // hadn't released, this would deadlock.)
        let mut token2: *const c_void = std::ptr::null();
        let begin2_rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                &mut token2,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(begin2_rc, 0);
        assert!(!token2.is_null());
        assert_ne!(token, token2, "tokens must be unique per begin call");

        let _ = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                handle,
                token2,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn full_access_callback_with_valid_token_resolves_scope() {
        // End-to-end: begin a scope, get a valid token, invoke a
        // FullAccess vtable callback with the token + a valid
        // descriptor. The callback's scope-token lookup must succeed
        // (no "invalid escalate scope" error). The actual allocation
        // may succeed or fail depending on the Vulkan environment
        // (render-target DMA-BUF availability, EGL DRM modifier
        // probe), but EITHER outcome proves the scope lookup passed:
        // a success returns rc=0 with `out_texture` populated; a
        // failure returns rc=1 with an error message that does NOT
        // contain "invalid escalate scope".
        //
        // (Mentally revert `with_full_scope_or_err` to always return
        // None — this test fails because the error message would
        // then contain "invalid escalate scope".)
        let Some((handle, _arc)) = make_host_handle() else {
            tracing::warn!(
                target: "streamlib::tests::escalate_vtable",
                "skipping valid-token test: no GPU device"
            );
            return;
        };

        let (mut buf, mut len) = make_err_buf();
        let mut token: *const c_void = std::ptr::null();
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                &mut token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            );
        }
        assert!(!token.is_null());

        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let mut buf2 = [0u8; 256];
        let mut len2 = 0usize;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE
                .acquire_render_target_dma_buf_image)(
                token,
                64,
                64,
                0, // Rgba8Unorm — valid format; forces scope lookup to run
                &mut out as *mut _ as *mut c_void,
                buf2.as_mut_ptr(),
                buf2.len(),
                &mut len2,
            )
        };

        if rc != 0 {
            // Allocation failed for an environment reason; assert the
            // failure was NOT a scope-lookup miss.
            let msg = err_buf_as_str(&buf2, len2);
            assert!(
                !msg.contains("invalid escalate scope"),
                "scope-token lookup must succeed inside an active \
                 scope; got: {msg}"
            );
        } else {
            // Allocation succeeded — definitively proves scope lookup
            // worked. The Texture in `out` owns a live handle; its
            // Drop will fire the vtable's drop_texture as the test
            // returns.
            assert!(!out.handle.is_null(), "out_texture handle populated");
            // SAFETY: `out` was overwritten by `ptr::write` from the
            // callback with a valid Texture; let its normal Drop run
            // to release the underlying handle via the vtable.
        }

        // Clean up the scope.
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                handle,
                token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            );
        }
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn full_access_callback_fails_after_escalate_end() {
        // Closes the scope-token validation loop: a token used after
        // escalate_end fires returns the InvalidEscalateScope error
        // (matches the "calls after escalate_end return
        // InvalidEscalateScope" exit criterion).
        let Some((handle, _arc)) = make_host_handle() else {
            tracing::warn!(
                target: "streamlib::tests::escalate_vtable",
                "skipping post-end test: no GPU device"
            );
            return;
        };

        let (mut buf, mut len) = make_err_buf();
        let mut token: *const c_void = std::ptr::null();
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                &mut token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            );
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                handle,
                token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            );
        }

        // Token is now stale — using it on any FullAccess callback
        // returns "invalid escalate scope".
        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let mut buf2 = [0u8; 256];
        let mut len2 = 0usize;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE
                .acquire_render_target_dma_buf_image)(
                token,
                64,
                64,
                0, // valid format
                &mut out as *mut _ as *mut c_void,
                buf2.as_mut_ptr(),
                buf2.len(),
                &mut len2,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf2, len2);
        assert!(
            msg.contains(
                "acquire_render_target_dma_buf_image: invalid escalate scope"
            ),
            "got: {msg}"
        );

        unsafe { free_host_handle(handle) };
    }
}
#[cfg(test)]
mod gpu_lim_texture_native_dma_buf_fd_tests {
    //! Tier-1 wire-format test for the Phase F
    //! `texture_native_dma_buf_fd` slot (#908 / #957). The slot is the
    //! cross-DSO landing for `Texture::native_handle` on Linux and
    //! returns the DMA-BUF FD widened to `i64`; sentinel `-1` encodes
    //! the `Option::None` case. A null texture handle must be a clean
    //! `-1` (no panic, no UB) — the wrapper short-circuits before any
    //! cast through `*const TextureInner`.

    use super::*;

    #[test]
    fn texture_native_dma_buf_fd_returns_minus_one_on_null_handle() {
        // Null texture_handle is the cdylib-shaped "Texture wasn't
        // minted yet / was already dropped" case. The slot returns
        // `-1` (= `Option::None` in the Rust-side wrapper) without
        // panicking and without touching the null pointer.
        let fd = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .texture_native_dma_buf_fd)(std::ptr::null())
        };
        assert_eq!(
            fd, -1,
            "null texture_handle must produce -1 sentinel (None)"
        );
    }
}
#[cfg(test)]
mod gpu_lim_video_source_timeline_semaphore_tests {
    //! Tier-1 wire-format tests for the v12 (#958)
    //! `set_video_source_timeline_semaphore` /
    //! `clear_video_source_timeline_semaphore` slots. Each wrapper
    //! must short-circuit on null gpu_handle (and `set` on null
    //! timeline_handle) without panicking and without dereferencing
    //! the null pointers.
    //!
    //! The non-null-handle path is exercised end-to-end by the
    //! `load_project_dylib_camera_smoke` integration test (which
    //! holds a real `Arc<HostVulkanTimelineSemaphore>` and is the
    //! only place a Tier-1 with-handle test could reach without
    //! constructing a real `GpuContext` here).
    //!
    //! Mental-revert: stub the wrapper bodies to
    //! `unimplemented!()` and these tests trip the underlying
    //! deref / panic — the wire-format claim regresses.
    use super::*;

    #[test]
    fn set_video_source_timeline_is_noop_on_null_gpu_handle() {
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .set_video_source_timeline_semaphore)(
                std::ptr::null(),
                std::ptr::null(),
            );
        }
    }

    // Note: the timeline_handle null guard at host_gpu_lim_set_video_source_timeline_semaphore
    // line 2078 isn't reachable at tier-1: the first guard
    // (handle_as_gpu_context) short-circuits on null gpu_handle, and
    // a non-null garbage gpu_handle would UB-deref before reaching
    // the timeline check. The guard is exercised end-to-end by
    // load_project_dylib_camera_smoke (the cdylib camera passes a
    // valid gpu_handle and a real Arc-borrow timeline_handle).

    #[test]
    fn clear_video_source_timeline_is_noop_on_null_gpu_handle() {
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .clear_video_source_timeline_semaphore)(std::ptr::null());
        }
    }

    /// v14 slot (#1066): tier-1 wire-format guard. Null `gpu_handle`
    /// must return null rather than dereferencing the pointer. The
    /// non-null-handle "slot empty" → null and "slot populated" →
    /// non-null Arc pointer paths are exercised end-to-end by the
    /// camera-display cdylib reproducer; a tier-1 unit test for them
    /// would need a real `GpuContext` instance, which this module
    /// deliberately avoids constructing.
    ///
    /// Mental-revert: stub the wrapper to `unimplemented!()` and
    /// this test trips the underlying panic — the null-guard
    /// contract regresses.
    #[test]
    fn host_video_source_timeline_arc_returns_null_on_null_gpu_handle() {
        let raw = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .host_video_source_timeline_arc)(std::ptr::null())
        };
        assert!(raw.is_null(), "expected null on null gpu_handle");
    }
}
#[cfg(test)]
mod gpu_lim_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for every callback on
    //! [`GpuContextLimitedAccessVTable`].
    //!
    //! Each test passes a null `handle` (and where applicable a null
    //! out-param or invalid input) and asserts the documented contract:
    //!
    //! - Lifecycle callbacks (clone/drop, Arc refcount bumps, etc.)
    //!   short-circuit on null and do not crash.
    //! - Probe callbacks (`strong_count_pixel_buffer`,
    //!   `plane_*_pixel_buffer`, `texture_registration_current_layout`,
    //!   etc.) return their documented default value.
    //! - Result-returning callbacks (`acquire_*`, `resolve_*`,
    //!   `command_queue`, `create_command_buffer*`, `blit_copy*`, ...)
    //!   return rc=1 with a callback-prefixed UTF-8 error in `err_buf`
    //!   and leave their out-slot unwritten.
    //! - `surface_store` writes a null-handle β-shape (the "None"
    //!   sentinel) regardless of input.
    //!
    //! `escalate_begin` / `escalate_end` are covered by
    //! [`gpu_lim_escalate_vtable_tests`]; `texture_native_dma_buf_fd`
    //! by [`gpu_lim_texture_native_dma_buf_fd_tests`].
    //!
    //! The vtable's `layout_version` field is locked against
    //! `GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION` so a
    //! cdylib-side ABI bump can't drift from the host's wiring.
    //!
    //! Tests that build a real `GpuContext` via `make_host_handle`
    //! carry `#[serial]` for the same NVIDIA dual-`VkDevice` reason
    //! as the escalate-vtable suite
    //! (`docs/learnings/nvidia-dual-vulkan-device-crash.md`).

    use super::*;
    use serial_test::serial;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    // ------------------------------------------------------------------
    // Layout-version match
    // ------------------------------------------------------------------

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.layout_version,
            streamlib_plugin_abi::GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION,
        );
    }

    // ------------------------------------------------------------------
    // Lifecycle callbacks — null is a documented no-op
    // ------------------------------------------------------------------

    /// Generates a `null_handle_no_crash` test for a single-argument
    /// lifecycle callback (clone/drop) that takes `handle: *const c_void`
    /// and returns `()` — null is documented as a no-op.
    macro_rules! null_handle_no_crash_test {
        ($test_name:ident, $field:ident) => {
            #[test]
            fn $test_name() {
                unsafe {
                    (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(std::ptr::null());
                }
            }
        };
    }

    null_handle_no_crash_test!(drop_handle_handles_null, drop_handle);
    null_handle_no_crash_test!(clone_pixel_buffer_handles_null, clone_pixel_buffer);
    null_handle_no_crash_test!(drop_pixel_buffer_handles_null, drop_pixel_buffer);
    null_handle_no_crash_test!(clone_texture_handles_null, clone_texture);
    null_handle_no_crash_test!(drop_texture_handles_null, drop_texture);
    null_handle_no_crash_test!(
        drop_pooled_texture_handle_handles_null,
        drop_pooled_texture_handle
    );
    null_handle_no_crash_test!(clone_storage_buffer_handles_null, clone_storage_buffer);
    null_handle_no_crash_test!(drop_storage_buffer_handles_null, drop_storage_buffer);
    null_handle_no_crash_test!(clone_uniform_buffer_handles_null, clone_uniform_buffer);
    null_handle_no_crash_test!(drop_uniform_buffer_handles_null, drop_uniform_buffer);
    null_handle_no_crash_test!(clone_vertex_buffer_handles_null, clone_vertex_buffer);
    null_handle_no_crash_test!(drop_vertex_buffer_handles_null, drop_vertex_buffer);
    null_handle_no_crash_test!(clone_index_buffer_handles_null, clone_index_buffer);
    null_handle_no_crash_test!(drop_index_buffer_handles_null, drop_index_buffer);
    null_handle_no_crash_test!(
        clone_texture_registration_handles_null,
        clone_texture_registration
    );
    null_handle_no_crash_test!(
        drop_texture_registration_handles_null,
        drop_texture_registration
    );
    null_handle_no_crash_test!(clone_rhi_command_queue_handles_null, clone_rhi_command_queue);
    null_handle_no_crash_test!(drop_rhi_command_queue_handles_null, drop_rhi_command_queue);
    null_handle_no_crash_test!(drop_command_buffer_handles_null, drop_command_buffer);
    null_handle_no_crash_test!(commit_command_buffer_handles_null, commit_command_buffer);
    null_handle_no_crash_test!(
        commit_and_wait_command_buffer_handles_null,
        commit_and_wait_command_buffer
    );

    // ------------------------------------------------------------------
    // Probe callbacks — null returns the documented sentinel
    // ------------------------------------------------------------------

    #[test]
    fn clone_handle_returns_null_on_null_input() {
        let out = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.clone_handle)(std::ptr::null())
        };
        assert!(out.is_null());
    }

    #[test]
    fn strong_count_pixel_buffer_returns_zero_on_null() {
        let n = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.strong_count_pixel_buffer)(
                std::ptr::null(),
            )
        };
        assert_eq!(n, 0);
    }

    #[test]
    fn plane_base_address_pixel_buffer_returns_null_on_null_handle() {
        let p = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.plane_base_address_pixel_buffer)(
                std::ptr::null(),
                0,
            )
        };
        assert!(p.is_null());
    }

    #[test]
    fn plane_size_pixel_buffer_returns_zero_on_null_handle() {
        let n = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.plane_size_pixel_buffer)(
                std::ptr::null(),
                0,
            )
        };
        assert_eq!(n, 0);
    }

    #[test]
    fn texture_registration_texture_returns_null_on_null_handle() {
        let p = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.texture_registration_texture)(
                std::ptr::null(),
            )
        };
        assert!(p.is_null());
    }

    #[test]
    fn texture_registration_current_layout_returns_zero_on_null_handle() {
        let v = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.texture_registration_current_layout)(
                std::ptr::null(),
            )
        };
        assert_eq!(v, 0, "VK_IMAGE_LAYOUT_UNDEFINED == 0");
    }

    #[test]
    fn texture_registration_update_layout_handles_null_no_crash() {
        // Two-arg shape (handle, layout_raw); null handle short-circuits
        // before the atomic store. The macro above is single-arg only,
        // so this gets its own test.
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.texture_registration_update_layout)(
                std::ptr::null(),
                42,
            );
        }
    }

    // ------------------------------------------------------------------
    // Update / register callbacks (no err_buf, no return) — null gpu
    // handle is a documented no-op
    // ------------------------------------------------------------------

    #[test]
    fn register_texture_handles_null_gpu_no_crash() {
        let id = b"abc";
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.register_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                std::ptr::null(),
                0,
            );
        }
    }

    #[test]
    fn update_texture_registration_layout_handles_null_gpu_no_crash() {
        let id = b"abc";
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.update_texture_registration_layout)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                42,
            );
        }
    }

    #[test]
    fn unregister_texture_handles_null_gpu_no_crash() {
        let id = b"abc";
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.unregister_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
            );
        }
    }

    #[test]
    fn copy_texture_command_buffer_handles_null_no_crash() {
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.copy_texture_command_buffer)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            );
        }
    }

    // ------------------------------------------------------------------
    // surface_store — always writes a defined β-shape; null gpu_handle
    // yields the "None" sentinel (null handle + null vtable)
    // ------------------------------------------------------------------

    #[test]
    fn surface_store_writes_null_beta_shape_on_null_gpu_handle() {
        // SAFETY: SurfaceStore is `#[repr(C)] (handle, vtable)`; the
        // callback always writes through the out-pointer first, so a
        // zero-init landing slot is safe to read after the call.
        let mut out: crate::core::context::SurfaceStore = unsafe { std::mem::zeroed() };
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.surface_store)(
                std::ptr::null(),
                &mut out as *mut _ as *mut c_void,
            );
        }
        assert!(out.is_none(), "null gpu_handle must produce a None β-shape");
    }

    #[test]
    fn surface_store_handles_null_out_param_no_crash() {
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.surface_store)(
                std::ptr::null(),
                std::ptr::null_mut(),
            );
        }
    }

    // ------------------------------------------------------------------
    // Result-returning callbacks (rc=1, err_buf populated)
    // ------------------------------------------------------------------

    /// Generates a null-gpu-handle test for a callback whose signature
    /// is `(gpu_handle, out, err_buf, err_buf_cap, err_len) -> i32` —
    /// the most common shape. `err_marker` is a substring expected in
    /// the err_buf message.
    macro_rules! null_gpu_handle_err_test {
        ($test_name:ident, $field:ident, $err_marker:expr) => {
            #[test]
            fn $test_name() {
                let (mut buf, mut len) = make_err_buf();
                let mut out_storage = [0u8; 256];
                let rc = unsafe {
                    (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(
                        std::ptr::null(),
                        out_storage.as_mut_ptr() as *mut c_void,
                        buf.as_mut_ptr(),
                        buf.len(),
                        &mut len,
                    )
                };
                assert_eq!(rc, 1);
                let msg = err_buf_as_str(&buf, len);
                assert!(msg.contains($err_marker), "got: {msg}");
            }
        };
    }

    null_gpu_handle_err_test!(
        command_queue_returns_error_on_null_gpu_handle,
        command_queue,
        "command_queue: null gpu handle"
    );

    null_gpu_handle_err_test!(
        create_command_buffer_returns_error_on_null_gpu_handle,
        create_command_buffer,
        "create_command_buffer: null gpu handle"
    );

    #[test]
    #[serial]
    fn command_queue_returns_error_on_null_out_param() {
        // null gpu_handle path runs first; need a non-null synthetic
        // handle to reach the null-out-param branch. Build a host-mode
        // handle if available; otherwise skip — this test is purely
        // about the wrapper's null-out-param guard, which on a null
        // gpu_handle is unreachable.
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.command_queue)(
                handle,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("command_queue: null out_queue"), "got: {msg}");
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn create_command_buffer_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.create_command_buffer)(
                handle,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("create_command_buffer: null out_cb"), "got: {msg}");
        unsafe { free_host_handle(handle) };
    }

    // --- acquire_texture ---

    #[test]
    fn acquire_texture_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_texture)(
                std::ptr::null(),
                64,
                64,
                0,
                0,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("acquire_texture: null gpu handle"), "got: {msg}");
    }

    #[test]
    #[serial]
    fn acquire_texture_returns_error_on_null_out_pooled_handle() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_texture)(
                handle,
                64,
                64,
                0,
                0,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_texture: null out_pooled_handle"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn acquire_texture_returns_error_on_invalid_format_raw() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_texture)(
                handle,
                64,
                64,
                99, // invalid format_raw
                0,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_texture: invalid format_raw"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- resolve_texture_by_surface_id ---

    #[test]
    fn resolve_texture_by_surface_id_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_texture_by_surface_id)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_texture_by_surface_id: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn resolve_texture_by_surface_id_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_texture_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_texture_by_surface_id: null out_texture"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn resolve_texture_by_surface_id_returns_error_on_invalid_utf8() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        // 0xFF, 0xFF, 0xFF is invalid UTF-8.
        let id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_texture_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_texture_by_surface_id: surface_id not valid UTF-8"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- resolve_texture_registration_by_surface_id ---

    #[test]
    fn resolve_texture_registration_by_surface_id_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .resolve_texture_registration_by_surface_id)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "resolve_texture_registration_by_surface_id: null gpu handle"
            ),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn resolve_texture_registration_by_surface_id_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .resolve_texture_registration_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "resolve_texture_registration_by_surface_id: null out_registration"
            ),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn resolve_texture_registration_by_surface_id_returns_error_on_invalid_utf8() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .resolve_texture_registration_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "resolve_texture_registration_by_surface_id: surface_id not valid UTF-8"
            ),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- acquire_{storage,uniform,vertex,index}_buffer ---
    // Linux: null gpu handle / null out_buffer → rc=1 + per-slot msg.
    // Non-Linux: always rc=1 + "not available on this platform".

    #[cfg(target_os = "linux")]
    mod buffer_acquire_linux {
        use super::*;

        macro_rules! buffer_acquire_null_gpu_test {
            ($test_name:ident, $field:ident, $err_marker:expr) => {
                #[test]
                fn $test_name() {
                    let (mut buf, mut len) = make_err_buf();
                    let mut out_storage = [0u8; 256];
                    let rc = unsafe {
                        (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(
                            std::ptr::null(),
                            1024,
                            out_storage.as_mut_ptr() as *mut c_void,
                            buf.as_mut_ptr(),
                            buf.len(),
                            &mut len,
                        )
                    };
                    assert_eq!(rc, 1);
                    let msg = err_buf_as_str(&buf, len);
                    assert!(msg.contains($err_marker), "got: {msg}");
                }
            };
        }

        buffer_acquire_null_gpu_test!(
            acquire_storage_buffer_returns_error_on_null_gpu_handle,
            acquire_storage_buffer,
            "acquire_storage_buffer: null gpu handle"
        );
        buffer_acquire_null_gpu_test!(
            acquire_uniform_buffer_returns_error_on_null_gpu_handle,
            acquire_uniform_buffer,
            "acquire_uniform_buffer: null gpu handle"
        );
        buffer_acquire_null_gpu_test!(
            acquire_vertex_buffer_returns_error_on_null_gpu_handle,
            acquire_vertex_buffer,
            "acquire_vertex_buffer: null gpu handle"
        );
        buffer_acquire_null_gpu_test!(
            acquire_index_buffer_returns_error_on_null_gpu_handle,
            acquire_index_buffer,
            "acquire_index_buffer: null gpu handle"
        );

        macro_rules! buffer_acquire_null_out_test {
            ($test_name:ident, $field:ident, $err_marker:expr) => {
                #[test]
                #[serial]
                fn $test_name() {
                    let Some((handle, _arc)) = make_host_handle() else {
                        return;
                    };
                    let (mut buf, mut len) = make_err_buf();
                    let rc = unsafe {
                        (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(
                            handle,
                            1024,
                            std::ptr::null_mut(),
                            buf.as_mut_ptr(),
                            buf.len(),
                            &mut len,
                        )
                    };
                    assert_eq!(rc, 1);
                    let msg = err_buf_as_str(&buf, len);
                    assert!(msg.contains($err_marker), "got: {msg}");
                    unsafe { free_host_handle(handle) };
                }
            };
        }

        buffer_acquire_null_out_test!(
            acquire_storage_buffer_returns_error_on_null_out_buffer,
            acquire_storage_buffer,
            "acquire_storage_buffer: null out_buffer"
        );
        buffer_acquire_null_out_test!(
            acquire_uniform_buffer_returns_error_on_null_out_buffer,
            acquire_uniform_buffer,
            "acquire_uniform_buffer: null out_buffer"
        );
        buffer_acquire_null_out_test!(
            acquire_vertex_buffer_returns_error_on_null_out_buffer,
            acquire_vertex_buffer,
            "acquire_vertex_buffer: null out_buffer"
        );
        buffer_acquire_null_out_test!(
            acquire_index_buffer_returns_error_on_null_out_buffer,
            acquire_index_buffer,
            "acquire_index_buffer: null out_buffer"
        );
    }

    #[cfg(not(target_os = "linux"))]
    mod buffer_acquire_non_linux {
        use super::*;

        macro_rules! buffer_acquire_not_available_test {
            ($test_name:ident, $field:ident, $err_marker:expr) => {
                #[test]
                fn $test_name() {
                    let (mut buf, mut len) = make_err_buf();
                    let mut out_storage = [0u8; 256];
                    let rc = unsafe {
                        (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(
                            std::ptr::null(),
                            1024,
                            out_storage.as_mut_ptr() as *mut c_void,
                            buf.as_mut_ptr(),
                            buf.len(),
                            &mut len,
                        )
                    };
                    assert_eq!(rc, 1);
                    let msg = err_buf_as_str(&buf, len);
                    assert!(msg.contains($err_marker), "got: {msg}");
                }
            };
        }

        buffer_acquire_not_available_test!(
            acquire_storage_buffer_reports_not_available,
            acquire_storage_buffer,
            "not available on this platform"
        );
        buffer_acquire_not_available_test!(
            acquire_uniform_buffer_reports_not_available,
            acquire_uniform_buffer,
            "not available on this platform"
        );
        buffer_acquire_not_available_test!(
            acquire_vertex_buffer_reports_not_available,
            acquire_vertex_buffer,
            "not available on this platform"
        );
        buffer_acquire_not_available_test!(
            acquire_index_buffer_reports_not_available,
            acquire_index_buffer,
            "not available on this platform"
        );
    }

    // --- create_command_buffer_from_queue ---

    #[test]
    fn create_command_buffer_from_queue_returns_error_on_null_queue_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.create_command_buffer_from_queue)(
                std::ptr::null(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_command_buffer_from_queue: null queue handle"),
            "got: {msg}"
        );
    }

    // --- copy_pixel_buffer_to_texture ---
    // Linux: tier-1 cover; non-Linux: stub returns "not available".

    #[cfg(target_os = "linux")]
    #[test]
    fn copy_pixel_buffer_to_texture_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.copy_pixel_buffer_to_texture)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("copy_pixel_buffer_to_texture: null gpu handle"),
            "got: {msg}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial]
    fn copy_pixel_buffer_to_texture_returns_error_on_null_pixel_buffer_or_texture() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.copy_pixel_buffer_to_texture)(
                handle,
                std::ptr::null(),
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "copy_pixel_buffer_to_texture: null pixel_buffer or texture"
            ),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn copy_pixel_buffer_to_texture_reports_not_available_on_non_linux() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.copy_pixel_buffer_to_texture)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("copy_pixel_buffer_to_texture: not available on this platform"),
            "got: {msg}"
        );
    }

    // --- blit_copy ---

    #[test]
    fn blit_copy_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.blit_copy)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("blit_copy: null gpu handle"), "got: {msg}");
    }

    #[test]
    #[serial]
    fn blit_copy_returns_error_on_null_src_or_dst() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.blit_copy)(
                handle,
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("blit_copy: null src or dst"), "got: {msg}");
        unsafe { free_host_handle(handle) };
    }

    // --- blit_copy_iosurface ---
    // macOS-only behaviour: null gpu / null dst → per-cause err.
    // Non-macOS: stub returns "not available on this platform (macOS-only)".

    #[cfg(target_os = "macos")]
    #[test]
    fn blit_copy_iosurface_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.blit_copy_iosurface)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("blit_copy_iosurface: null gpu handle"),
            "got: {msg}"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn blit_copy_iosurface_reports_not_available_on_non_macos() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.blit_copy_iosurface)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("blit_copy_iosurface: not available on this platform"),
            "got: {msg}"
        );
    }

    // --- check_out_surface ---

    #[test]
    fn check_out_surface_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.check_out_surface)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("check_out_surface: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn check_out_surface_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.check_out_surface)(
                handle,
                id.as_ptr(),
                id.len(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("check_out_surface: null out_pixel_buffer"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn check_out_surface_returns_error_on_invalid_utf8() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.check_out_surface)(
                handle,
                id.as_ptr(),
                id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("check_out_surface: surface_id not valid UTF-8"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- acquire_pixel_buffer ---

    #[test]
    fn acquire_pixel_buffer_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_pixel_buffer)(
                std::ptr::null(),
                64,
                64,
                0x42475241, // valid Bgra32
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_pixel_buffer: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn acquire_pixel_buffer_returns_error_on_null_out_pixel_buffer() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_pixel_buffer)(
                handle,
                64,
                64,
                0x42475241,
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_pixel_buffer: null out_pixel_buffer"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn acquire_pixel_buffer_returns_error_on_invalid_format_raw() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_pixel_buffer)(
                handle,
                64,
                64,
                0xDEAD_BEEF, // invalid format_raw
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_pixel_buffer: invalid format_raw"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- get_pixel_buffer ---

    #[test]
    fn get_pixel_buffer_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let pool_id = b"pool-x";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.get_pixel_buffer)(
                std::ptr::null(),
                pool_id.as_ptr(),
                pool_id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("get_pixel_buffer: null gpu handle"), "got: {msg}");
    }

    #[test]
    #[serial]
    fn get_pixel_buffer_returns_error_on_null_out_pixel_buffer() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let pool_id = b"pool-x";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.get_pixel_buffer)(
                handle,
                pool_id.as_ptr(),
                pool_id.len(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("get_pixel_buffer: null out_pixel_buffer"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn get_pixel_buffer_returns_error_on_invalid_utf8_pool_id() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let pool_id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.get_pixel_buffer)(
                handle,
                pool_id.as_ptr(),
                pool_id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("get_pixel_buffer: pool_id not valid UTF-8"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- resolve_pixel_buffer_by_surface_id ---

    #[test]
    fn resolve_pixel_buffer_by_surface_id_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_pixel_buffer_by_surface_id)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_pixel_buffer_by_surface_id: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn resolve_pixel_buffer_by_surface_id_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_pixel_buffer_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_pixel_buffer_by_surface_id: null out_pixel_buffer"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn resolve_pixel_buffer_by_surface_id_returns_error_on_invalid_utf8() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_pixel_buffer_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "resolve_pixel_buffer_by_surface_id: surface_id not valid UTF-8"
            ),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // ------------------------------------------------------------------
    // Helpers — build a host-mode `gpu_handle` so the null-out / invalid-
    // input branches downstream of the null-handle guard can fire.
    //
    // Tests that take a real GpuContext are inherently unsafe in the
    // workspace lib suite when other tests construct VkDevices
    // concurrently (NVIDIA dual-VkDevice SIGSEGV per
    // `docs/learnings/nvidia-dual-vulkan-device-crash.md`). The
    // escalate-vtable tests use `#[serial]` for that reason. Tier-1
    // wire-format checks here either pass `null` (no GpuContext needed)
    // or build a fresh GpuContext per test — the latter case is
    // tolerated to be skipped via `init_for_platform` returning Err on
    // hosts without a GPU; subsequent calls then short-circuit the
    // test via early `return`. The host-handle-using tests do NOT race
    // because they never create a second VkDevice concurrently with the
    // serial escalate suite — the same `make_host_handle` shape used
    // there is reused here for symmetry.
    // ------------------------------------------------------------------

    fn make_host_handle() -> Option<(*const c_void, Arc<crate::core::context::GpuContext>)> {
        let gpu = crate::core::context::GpuContext::init_for_platform().ok()?;
        let arc = Arc::new(gpu);
        let boxed: Box<Arc<crate::core::context::GpuContext>> = Box::new(Arc::clone(&arc));
        let handle = Box::into_raw(boxed) as *const c_void;
        Some((handle, arc))
    }

    unsafe fn free_host_handle(handle: *const c_void) {
        let _ = unsafe {
            Box::from_raw(handle as *mut Arc<crate::core::context::GpuContext>)
        };
    }
}
