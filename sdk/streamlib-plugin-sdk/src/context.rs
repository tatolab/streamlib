// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twins of the engine's capability-typed context views.
//!
//! These are `#[repr(C)]` layout-matched copies of the engine's
//! [`RuntimeContextFullAccess`] / [`RuntimeContextLimitedAccess`] /
//! [`GpuContextFullAccess`] / [`GpuContextLimitedAccess`]. The host
//! constructs a view, passes `&view as *const _ as *const c_void`
//! across the plugin ABI, and the cdylib casts it straight back —
//! reading the host-built struct's fields directly. That is sound only
//! because both sides compile the SAME `#[repr(C)]` layout. The layout
//! tests below pin the byte shape against the engine's identical
//! assertions; a field added to one side but not the other trips a test
//! rather than corrupting field reads at runtime.
//!
//! Beyond the GPU-accessor field reads, the runtime-context views also
//! surface the host's audio clock via `audio_clock()` — dispatched
//! through the [`RuntimeContextVTable::audio_clock_handle`] slot paired
//! with the host's `AudioClockVTable` cached on `HostServices` — and the
//! host-owned identifier / lifecycle-flag accessors `runtime_id()`,
//! `processor_id()`, `is_paused()`, and `should_process()`, dispatched
//! through the matching [`RuntimeContextVTable`] slots. The one remaining
//! ABI-mediated accessor (`runtime`, whose `RuntimeOpsVTable` shim carries
//! no transport for host-owned streaming ops) is a later phase.

use std::ffi::c_void;
use std::marker::PhantomData;

use streamlib_plugin_abi::{
    GpuContextFullAccessVTable, GpuContextLimitedAccessVTable, RuntimeContextVTable,
};

use crate::audio_clock_shim::AudioClockShim;

#[cfg(target_os = "linux")]
use streamlib_consumer_rhi::{PixelFormat, TextureFormat, TextureUsages, VulkanLayout};
#[cfg(target_os = "linux")]
use streamlib_error::{Error, Result};
#[cfg(target_os = "linux")]
use streamlib_plugin_abi::GpuCapabilitiesRepr;

// =============================================================================
// GpuContextLimitedAccess — cdylib arm
// =============================================================================

/// Restricted GPU capability shim with ABI-stable `(handle, vtable)`
/// shape. Cdylib-arm twin of the engine's `GpuContextLimitedAccess`.
#[repr(C)]
pub struct GpuContextLimitedAccess {
    pub(crate) handle: *const c_void,
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
}

// SAFETY: `handle` points at a host-owned `Box<Arc<GpuContext>>` that is
// `Send + Sync`; the vtable pointer is `&'static` for the host's lifetime.
// Every method reaches the GpuContext through the handle via the vtable.
unsafe impl Send for GpuContextLimitedAccess {}
unsafe impl Sync for GpuContextLimitedAccess {}

impl Clone for GpuContextLimitedAccess {
    /// plugin-ABI-safe Clone. Dispatches through
    /// [`GpuContextLimitedAccessVTable::clone_handle`] to bump the
    /// host's `Arc<GpuContext>` refcount.
    fn clone(&self) -> Self {
        let new_handle = if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: handle + vtable were paired at construction and the
            // host's `clone_handle` callback contractually returns a fresh
            // owning pointer the matching `drop_handle` releases.
            unsafe { ((*self.vtable).clone_handle)(self.handle) }
        } else {
            std::ptr::null()
        };
        Self {
            handle: new_handle,
            vtable: self.vtable,
        }
    }
}

impl Drop for GpuContextLimitedAccess {
    /// Releases the host-owned handle via
    /// [`GpuContextLimitedAccessVTable::drop_handle`].
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: handle was produced by the host's `new()` /
            // `clone_handle`; the matching `drop_handle` callback runs
            // `Box::from_raw + drop` on the host side.
            unsafe { ((*self.vtable).drop_handle)(self.handle) };
        }
    }
}

#[cfg(target_os = "linux")]
impl GpuContextLimitedAccess {
    /// Acquire a HOST_VISIBLE storage buffer for CPU→GPU SSBO upload.
    /// Dispatches through the plugin ABI vtable's `acquire_storage_buffer`
    /// callback.
    pub fn acquire_storage_buffer(&self, byte_size: u64) -> Result<crate::rhi::StorageBuffer> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "acquire_storage_buffer: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut out: std::mem::MaybeUninit<crate::rhi::StorageBuffer> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle were paired at construction; `out`
        // points at uninitialized stack storage the host writes a valid
        // `StorageBuffer` into on success.
        let status = unsafe {
            ((*self.vtable).acquire_storage_buffer)(
                self.handle,
                byte_size,
                out.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid value.
            Ok(unsafe { out.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Update a registered texture's tracked layout after a transition.
    /// Dispatches through the plugin ABI vtable's
    /// `update_texture_registration_layout` callback.
    pub fn update_texture_registration_layout(&self, id: &str, layout: VulkanLayout) {
        if self.handle.is_null() || self.vtable.is_null() {
            return;
        }
        // SAFETY: handle + vtable were paired at construction.
        unsafe {
            ((*self.vtable).update_texture_registration_layout)(
                self.handle,
                id.as_ptr(),
                id.len(),
                layout.0,
            );
        }
    }

    /// Resolve an incoming `VideoFrame`'s `surface_id` to a
    /// [`TextureRegistration`](crate::rhi::TextureRegistration) — the GPU
    /// texture plus its last-known layout. This is the engine-free
    /// surface-consumer entry point: a plugin's `process()` calls it to read
    /// a decoded frame on the GPU and run a compute/graphics kernel on it.
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `resolve_texture_registration_by_surface_id` callback. `texture_layout`
    /// is the optional per-frame layout override carried on the `VideoFrame`
    /// (raw `VkImageLayout`); pass `None` to use the per-surface default.
    pub fn resolve_texture_registration_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<crate::rhi::TextureRegistration> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "resolve_texture_registration_by_surface_id: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut out_reg: std::mem::MaybeUninit<crate::rhi::TextureRegistration> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        let (has_layout, layout_raw) = match texture_layout {
            Some(v) => (1i32, v),
            None => (0i32, 0i32),
        };
        // SAFETY: handle + vtable were paired at construction; `out_reg` is
        // uninitialized stack storage the host writes a valid
        // TextureRegistration into on success (status == 0) and nothing on
        // failure.
        let status = unsafe {
            ((*self.vtable).resolve_texture_registration_by_surface_id)(
                self.handle,
                surface_id.as_ptr(),
                surface_id.len(),
                has_layout,
                layout_raw,
                width,
                height,
                out_reg.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid value.
            Ok(unsafe { out_reg.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Resolve an incoming `VideoFrame`'s `surface_id` directly to a
    /// [`Texture`](crate::rhi::Texture) (the texture-cache fast path, without
    /// the surrounding [`TextureRegistration`](crate::rhi::TextureRegistration)
    /// layout metadata).
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `resolve_texture_by_surface_id` callback.
    pub fn resolve_texture_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<crate::rhi::Texture> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "resolve_texture_by_surface_id: GpuContextLimitedAccess has null handle/vtable"
                    .into(),
            ));
        }
        let mut out_texture: std::mem::MaybeUninit<crate::rhi::Texture> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        let (has_layout, layout_raw) = match texture_layout {
            Some(v) => (1i32, v),
            None => (0i32, 0i32),
        };
        // SAFETY: handle + vtable were paired at construction; `out_texture`
        // points at uninitialized stack storage the host writes a valid
        // `Texture` into on success (status == 0) and nothing on failure.
        let status = unsafe {
            ((*self.vtable).resolve_texture_by_surface_id)(
                self.handle,
                surface_id.as_ptr(),
                surface_id.len(),
                has_layout,
                layout_raw,
                width,
                height,
                out_texture.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid value.
            Ok(unsafe { out_texture.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Check out a shared surface (CPU-readable [`PixelBuffer`](crate::rhi::PixelBuffer))
    /// by its `surface_id`. Dispatches through the plugin ABI vtable's
    /// `check_out_surface` callback.
    pub fn check_out_surface(&self, surface_id: &str) -> Result<crate::rhi::PixelBuffer> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "check_out_surface: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut out_pb: std::mem::MaybeUninit<crate::rhi::PixelBuffer> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable paired at construction; the host writes a
        // valid PixelBuffer into `out_pb` on success.
        let status = unsafe {
            ((*self.vtable).check_out_surface)(
                self.handle,
                surface_id.as_ptr(),
                surface_id.len(),
                out_pb.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(unsafe { out_pb.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Resolve an incoming `VideoFrame`'s `surface_id` to a CPU-readable
    /// [`PixelBuffer`](crate::rhi::PixelBuffer). Dispatches through the plugin
    /// ABI vtable's `resolve_pixel_buffer_by_surface_id` callback.
    pub fn resolve_pixel_buffer_by_surface_id(
        &self,
        surface_id: &str,
    ) -> Result<crate::rhi::PixelBuffer> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "resolve_pixel_buffer_by_surface_id: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut out_pb: std::mem::MaybeUninit<crate::rhi::PixelBuffer> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable paired at construction; the host writes a
        // valid PixelBuffer into `out_pb` on success.
        let status = unsafe {
            ((*self.vtable).resolve_pixel_buffer_by_surface_id)(
                self.handle,
                surface_id.as_ptr(),
                surface_id.len(),
                out_pb.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(unsafe { out_pb.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Acquire a pooled [`PixelBuffer`](crate::rhi::PixelBuffer) for CPU→GPU
    /// upload, returning its pool id (hand back to [`Self::get_pixel_buffer`]).
    /// Dispatches through the plugin ABI vtable's `acquire_pixel_buffer`
    /// callback.
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(crate::rhi::PixelBufferPoolId, crate::rhi::PixelBuffer)> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "acquire_pixel_buffer: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut pool_id_buf = [0u8; 1024];
        let mut pool_id_len: usize = 0;
        let mut out_pb: std::mem::MaybeUninit<crate::rhi::PixelBuffer> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable paired at construction; the host writes the
        // pool-id UTF-8 bytes into `pool_id_buf` and a valid PixelBuffer into
        // `out_pb` on success.
        let status = unsafe {
            ((*self.vtable).acquire_pixel_buffer)(
                self.handle,
                width,
                height,
                format as u32,
                pool_id_buf.as_mut_ptr(),
                pool_id_buf.len(),
                &mut pool_id_len as *mut usize,
                out_pb.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            let id_str =
                String::from_utf8_lossy(&pool_id_buf[..pool_id_len.min(pool_id_buf.len())])
                    .into_owned();
            let pool_id = crate::rhi::PixelBufferPoolId::from_string(id_str);
            let pb = unsafe { out_pb.assume_init() };
            Ok((pool_id, pb))
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Look up a previously-acquired pooled
    /// [`PixelBuffer`](crate::rhi::PixelBuffer) by its pool id. Dispatches
    /// through the plugin ABI vtable's `get_pixel_buffer` callback.
    pub fn get_pixel_buffer(
        &self,
        pool_id: &crate::rhi::PixelBufferPoolId,
    ) -> Result<crate::rhi::PixelBuffer> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "get_pixel_buffer: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let id_str = pool_id.as_str();
        let mut out_pb: std::mem::MaybeUninit<crate::rhi::PixelBuffer> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable paired at construction.
        let status = unsafe {
            ((*self.vtable).get_pixel_buffer)(
                self.handle,
                id_str.as_ptr(),
                id_str.len(),
                out_pb.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(unsafe { out_pb.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Register a texture under a `surface_id` (defaults to layout UNDEFINED).
    /// Dispatches through the plugin ABI vtable's `register_texture` callback;
    /// the host bumps the texture's Arc before stashing it, so the passed
    /// [`Texture`](crate::rhi::Texture) is consumed.
    pub fn register_texture(&self, id: &str, texture: crate::rhi::Texture) {
        if self.handle.is_null() || self.vtable.is_null() {
            return;
        }
        // SAFETY: handle + vtable paired at construction; `texture.handle` is
        // a live `Arc::into_raw(Arc<TextureInner>)` the host re-bumps.
        unsafe {
            ((*self.vtable).register_texture)(
                self.handle,
                id.as_ptr(),
                id.len(),
                texture.handle,
                0, // VulkanLayout::UNDEFINED.0 == 0
            );
        }
        drop(texture);
    }

    /// Register a texture under a `surface_id` with an explicit initial layout.
    /// Dispatches through the plugin ABI vtable's `register_texture` callback.
    pub fn register_texture_with_layout(
        &self,
        id: &str,
        texture: crate::rhi::Texture,
        initial_layout: VulkanLayout,
    ) {
        if self.handle.is_null() || self.vtable.is_null() {
            return;
        }
        // SAFETY: handle + vtable paired at construction; `texture.handle` is
        // a live `Arc::into_raw(Arc<TextureInner>)` the host re-bumps.
        unsafe {
            ((*self.vtable).register_texture)(
                self.handle,
                id.as_ptr(),
                id.len(),
                texture.handle,
                initial_layout.0,
            );
        }
        drop(texture);
    }

    /// Unregister a texture by its `surface_id`. Dispatches through the plugin
    /// ABI vtable's `unregister_texture` callback. Idempotent.
    pub fn unregister_texture(&self, id: &str) {
        if self.handle.is_null() || self.vtable.is_null() {
            return;
        }
        // SAFETY: handle + vtable paired at construction.
        unsafe {
            ((*self.vtable).unregister_texture)(self.handle, id.as_ptr(), id.len());
        }
    }

    /// Obtain the host's cross-process
    /// [`SurfaceStore`](crate::rhi::SurfaceStore) producer handle. The
    /// host always writes a value: a live handle when it ships a surface
    /// store, or a null-handle sentinel otherwise — branch on
    /// [`SurfaceStore::is_none`](crate::rhi::SurfaceStore::is_none).
    /// Dispatches through the plugin ABI vtable's `surface_store` callback.
    pub fn surface_store(&self) -> crate::rhi::SurfaceStore {
        if self.handle.is_null() || self.vtable.is_null() {
            return crate::rhi::SurfaceStore {
                handle: std::ptr::null(),
                vtable: std::ptr::null(),
            };
        }
        let mut out: std::mem::MaybeUninit<crate::rhi::SurfaceStore> =
            std::mem::MaybeUninit::uninit();
        // SAFETY: vtable + handle paired at construction; the host always
        // writes a fully-initialized (possibly null-handle) SurfaceStore
        // PluginAbiObject into `out`.
        unsafe {
            ((*self.vtable).surface_store)(self.handle, out.as_mut_ptr() as *mut c_void);
            out.assume_init()
        }
    }

    /// Acquire a pooled scratch texture, returning a
    /// [`PooledTextureHandle`](crate::rhi::PooledTextureHandle) that returns
    /// the texture to the pool on Drop. Dispatches through the plugin ABI
    /// vtable's `acquire_texture` callback.
    pub fn acquire_texture(
        &self,
        desc: &crate::rhi::TexturePoolDescriptor,
    ) -> Result<crate::rhi::PooledTextureHandle> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "acquire_texture: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut out_pooled: std::mem::MaybeUninit<crate::rhi::PooledTextureHandle> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable paired at construction; the host writes a
        // valid PooledTextureHandle into `out_pooled` on success.
        let status = unsafe {
            ((*self.vtable).acquire_texture)(
                self.handle,
                desc.width,
                desc.height,
                desc.format as u32,
                desc.usage.bits(),
                out_pooled.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(unsafe { out_pooled.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Borrow a privileged [`GpuContextFullAccess`] for the duration of
    /// `f`, then drain the device and release the host's escalate gate.
    ///
    /// This is a `process()` body's only path to privileged GPU
    /// resource-creation (readbacks, texture rings, encoder sessions): the
    /// host holds its escalate gate across the closure, then `escalate_end`
    /// drains the device (`wait_device_idle`) inside the gate and releases
    /// it. A closure panic still runs `escalate_end` — the gate never
    /// leaks — and then re-raises. Returns [`Error::EscalateBeginRejected`]
    /// when the host refuses the scope; the actionable cause is a nested
    /// `escalate` or an `escalate` inside a FullAccess lifecycle body
    /// (`setup` / `teardown` / Manual `start` / `stop`), both same-thread
    /// gate re-entry — call `ctx.gpu_full_access().X()` directly in those
    /// bodies instead.
    ///
    /// Two soft contracts the borrow checker does not enforce:
    /// - Do not spawn a thread that outlives the closure while it still
    ///   holds `full` — the scope token is invalidated the instant
    ///   `escalate` returns, so an in-flight FullAccess call from an
    ///   escaped thread races `escalate_end`.
    /// - Ending the scope costs a device-wide drain, so do not `escalate`
    ///   per frame. Escalate once (first frame, or an extent change), cache
    ///   the created resource, and run per-frame work through that
    ///   resource's own methods (its `submit` / `try_read` are scope-free).
    ///
    /// ```compile_fail
    /// fn escapes(limited: &streamlib_plugin_sdk::sdk::context::GpuContextLimitedAccess) {
    ///     // The closure's `&GpuContextFullAccess` is HRTB-scoped to the
    ///     // escalate window; returning it out of the closure cannot escape
    ///     // — this fails to compile.
    ///     let _escaped = limited.escalate(|full| full);
    /// }
    /// ```
    pub fn escalate<R>(&self, f: impl FnOnce(&GpuContextFullAccess) -> R) -> Result<R> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "escalate: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        // The FullAccess vtable is ABI-optional (HostServices v6): null on a
        // host that ships no GpuContext. Check it BEFORE `escalate_begin` so
        // the GPU-less-host arm never enters — and so never has to leave —
        // the escalate gate (no cleanup path needed on this error arm).
        let full_vtable = crate::plugin::host_callbacks()
            .map(|callbacks| callbacks.gpu_context_full_access_vtable)
            .unwrap_or(std::ptr::null());
        if full_vtable.is_null() {
            return Err(Error::GpuError(
                "escalate: host did not install a GpuContextFullAccessVTable (GPU-less host)"
                    .into(),
            ));
        }
        // SAFETY: handle + vtable were paired at construction; the vtable is
        // `&'static` for the host's lifetime.
        let vt = unsafe { &*self.vtable };

        let mut scope_token: *const c_void = std::ptr::null();
        let mut begin_err_buf = [0u8; 512];
        let mut begin_err_len: usize = 0;
        // SAFETY: handle + vtable paired at construction; `scope_token` and
        // the err buffer are live stack storage the host writes the opaque
        // token / error bytes into.
        let begin_rc = unsafe {
            (vt.escalate_begin)(
                self.handle,
                &mut scope_token,
                begin_err_buf.as_mut_ptr(),
                begin_err_buf.len(),
                &mut begin_err_len as *mut usize,
            )
        };
        if begin_rc != 0 {
            // Named variant: the host refused escalate_begin — same-thread
            // gate re-entry (nested escalate / escalate inside a FullAccess
            // lifecycle body), caught at the boundary. The host's err_buf
            // carries the actionable message.
            let msg =
                String::from_utf8_lossy(&begin_err_buf[..begin_err_len.min(begin_err_buf.len())])
                    .into_owned();
            return Err(Error::EscalateBeginRejected(msg));
        }

        // Scope-token FullAccess: the opaque token as `handle`, the host's
        // FullAccess vtable, borrowing (no refcount bump) the originating
        // LimitedAccess pair for the Option-B mirror-method dispatch. The
        // borrow is sound because we hold `&self` across the closure. Drop
        // is a no-op in ScopeToken mode — `escalate_end` below is the single
        // release authority.
        let full = GpuContextFullAccess {
            handle: scope_token,
            vtable: full_vtable,
            handle_kind: HandleKind::ScopeToken,
            inherited_lim_handle: self.handle,
            inherited_lim_vtable: self.vtable,
        };
        // catch_unwind so a closure panic still fires escalate_end (below) —
        // otherwise the host's escalate gate would leak. AssertUnwindSafe
        // mirrors the engine twin: `escalate`'s signature adds no
        // `UnwindSafe` bound on the closure. NOTHING fallible (`?`) runs
        // between a successful `escalate_begin` and `escalate_end`.
        let closure_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&full)));
        drop(full);

        // UNCONDITIONAL: `escalate_end` is the single release authority — it
        // drains the device (`wait_device_idle`) WHILE holding the gate,
        // then releases it and invalidates the token. Runs on both the Ok
        // and the panic path.
        let mut end_err_buf = [0u8; 512];
        let mut end_err_len: usize = 0;
        // SAFETY: same (handle, vtable) pair; `scope_token` is the token
        // minted by `escalate_begin` above.
        let end_rc = unsafe {
            (vt.escalate_end)(
                self.handle,
                scope_token,
                end_err_buf.as_mut_ptr(),
                end_err_buf.len(),
                &mut end_err_len as *mut usize,
            )
        };

        match closure_result {
            Ok(value) => {
                if end_rc != 0 {
                    let msg =
                        String::from_utf8_lossy(&end_err_buf[..end_err_len.min(end_err_buf.len())])
                            .into_owned();
                    Err(Error::GpuError(format!(
                        "escalate: escalate_end failed: {msg}"
                    )))
                } else {
                    Ok(value)
                }
            }
            // The closure unwound; `escalate_end` already fired (gate
            // released). Re-raise the original panic so the plugin's
            // `process()` sees the panic, not a swallowed error.
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
}

// =============================================================================
// HandleKind — drop discriminator on GpuContextFullAccess
// =============================================================================

/// Discriminator for [`GpuContextFullAccess`]'s `handle` field. The
/// engine-internal in-process constructor sets `Boxed`; the cdylib
/// vtable-dispatched constructor sets `ScopeToken`. Drop dispatches on
/// this kind.
// `GpuContextLimitedAccess::escalate` is the SDK's only constructor of a
// `HandleKind` — it builds a `ScopeToken` view for the escalate closure.
// The `Boxed` variant is host-only (the SDK never mints a
// `Box<Arc<GpuContext>>` handle); it exists for `#[repr(C)]` layout parity
// and the Drop match, so the allow keeps its never-constructed state quiet.
#[allow(dead_code)]
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandleKind {
    /// Handle is a host-allocated `Box<Arc<GpuContext>>`. The SDK never
    /// constructs this variant — only the host does.
    Boxed = 0,
    /// Handle is an opaque scope token from the host's
    /// `GpuContextLimitedAccessVTable::escalate_begin` callback.
    ScopeToken = 1,
}

// =============================================================================
// GpuContextFullAccess — cdylib arm
// =============================================================================

/// Privileged GPU capability shim with ABI-stable shape. Cdylib-arm twin
/// of the engine's `GpuContextFullAccess`.
///
/// Deliberately **not** `Clone` — a `&GpuContextFullAccess` is borrowed
/// from a [`RuntimeContextFullAccess`] for the duration of a single
/// lifecycle call, or from [`GpuContextLimitedAccess::escalate`] for the
/// duration of the closure, and cannot be stashed.
///
/// ```compile_fail
/// fn assert_not_clone<T: Clone>() {}
/// assert_not_clone::<streamlib_plugin_sdk::sdk::context::GpuContextFullAccess>();
/// ```
#[repr(C)]
pub struct GpuContextFullAccess {
    pub(crate) handle: *const c_void,
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Drop discriminator. The cdylib only ever receives
    /// [`HandleKind::ScopeToken`] instances (built by the host's escalate
    /// path); the [`HandleKind::Boxed`] arm exists only for layout parity.
    pub(crate) handle_kind: HandleKind,
    /// Inherited LimitedAccess handle (scope-token mode only). `null` in
    /// Boxed mode.
    pub(crate) inherited_lim_handle: *const c_void,
    /// Inherited LimitedAccess vtable pointer paired with
    /// [`Self::inherited_lim_handle`]. `null` in Boxed mode.
    pub(crate) inherited_lim_vtable: *const GpuContextLimitedAccessVTable,
}

// SAFETY: same shape as the engine twin. The handle is a host-owned
// `Box<Arc<GpuContext>>` or an opaque scope token (both `Send + Sync`);
// the vtable pointers are `&'static`; the inherited LimitedAccess fields
// either borrow the originating LimitedAccess's host handle or are null.
unsafe impl Send for GpuContextFullAccess {}
unsafe impl Sync for GpuContextFullAccess {}

impl Drop for GpuContextFullAccess {
    /// Releases the handle.
    ///
    /// The cdylib only ever holds [`HandleKind::ScopeToken`] instances,
    /// whose cleanup is the authority of the host's `escalate_end`
    /// callback — so Drop is a no-op here. The [`HandleKind::Boxed`] arm
    /// is unreachable in the SDK (the SDK never constructs a Boxed
    /// handle), so it is also a no-op rather than naming the engine's
    /// `Arc<GpuContext>`.
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        match self.handle_kind {
            // The SDK never constructs a Boxed handle (that requires the
            // engine's `Arc<GpuContext>`). Unreachable in cdylib code.
            HandleKind::Boxed => {}
            // No-op — escalate_end is the authority.
            HandleKind::ScopeToken => {}
        }
    }
}

/// Host GPU capability snapshot — the Rust-side projection of the plugin
/// ABI's `GpuCapabilitiesRepr`, read once at processor setup for
/// device-vendor branching and external-memory / cross-device-DMA-BUF
/// probe checks. Returned by
/// [`GpuContextFullAccess::gpu_capabilities`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuCapabilities {
    /// GPU device / vendor name (`VkPhysicalDeviceProperties::deviceName`).
    pub device_name: String,
    /// Whether the GPU exposes `VK_KHR_external_memory_fd` +
    /// `VK_EXT_external_memory_dma_buf` (DMA-BUF FD import available).
    pub supports_external_memory: bool,
    /// Whether cross-device DMA-BUF probe is supported (NVIDIA Linux
    /// reports `false` per the engine-layer capability guard).
    pub supports_cross_device_dma_buf_probe: bool,
    /// Whether the GPU exposes `VK_KHR_ray_tracing_pipeline`.
    pub supports_ray_tracing_pipeline: bool,
}

/// Project a `GpuCapabilitiesRepr` (fixed-size UTF-8 device-name buffer +
/// valid-length + `u8` capability bools) into the Rust-side
/// [`GpuCapabilities`].
#[cfg(target_os = "linux")]
fn gpu_capabilities_from_repr(repr: &GpuCapabilitiesRepr) -> GpuCapabilities {
    let name_len = (repr.device_name_len as usize).min(repr.device_name.len());
    let device_name = String::from_utf8_lossy(&repr.device_name[..name_len]).into_owned();
    GpuCapabilities {
        device_name,
        supports_external_memory: repr.supports_external_memory != 0,
        supports_cross_device_dma_buf_probe: repr.supports_cross_device_dma_buf_probe != 0,
        supports_ray_tracing_pipeline: repr.supports_ray_tracing_pipeline != 0,
    }
}

// =============================================================================
// GpuContextFullAccess — privileged GPU surface (cdylib / ScopeToken arm)
// =============================================================================
//
// The SDK only ever holds a ScopeToken-mode FullAccess (built by the
// host's `escalate_begin` path), so each method carries ONLY the
// vtable-dispatch arm — the engine's `HandleKind::Boxed => host_inner()…`
// arm is dropped (the SDK never constructs a Boxed handle). The
// LimitedAccess-mirror methods (`acquire_storage_buffer`,
// `update_texture_registration_layout`) inherit through the originating
// LimitedAccess vtable per the `inherited_lim_*` fields, mirroring the
// engine's Option-B dispatch.

#[cfg(target_os = "linux")]
impl GpuContextFullAccess {
    /// Construct a non-dropping view of the originating
    /// [`GpuContextLimitedAccess`] for cdylib dispatch through the
    /// inherited vtable.
    ///
    /// **Wrapped in [`std::mem::ManuallyDrop`]** so the borrowed handle
    /// isn't double-released — the originating LimitedAccess outlives the
    /// FullAccess scope and owns the only Drop responsibility for the
    /// handle.
    pub(crate) fn inherited_limited_unchecked(
        &self,
    ) -> std::mem::ManuallyDrop<GpuContextLimitedAccess> {
        std::mem::ManuallyDrop::new(GpuContextLimitedAccess {
            handle: self.inherited_lim_handle,
            vtable: self.inherited_lim_vtable,
        })
    }

    /// Acquire a HOST_VISIBLE storage buffer for CPU→GPU SSBO upload.
    ///
    /// LimitedAccess mirror — cdylib dispatch inherits the
    /// `acquire_storage_buffer` slot via
    /// [`Self::inherited_limited_unchecked`].
    pub fn acquire_storage_buffer(&self, byte_size: u64) -> Result<crate::rhi::StorageBuffer> {
        self.inherited_limited_unchecked()
            .acquire_storage_buffer(byte_size)
    }

    /// Update a registered texture's tracked layout after a transition.
    ///
    /// LimitedAccess mirror — cdylib dispatch inherits the
    /// `update_texture_registration_layout` slot via
    /// [`Self::inherited_limited_unchecked`].
    pub fn update_texture_registration_layout(&self, id: &str, layout: VulkanLayout) {
        self.inherited_limited_unchecked()
            .update_texture_registration_layout(id, layout);
    }

    /// Resolve an incoming `VideoFrame`'s `surface_id` to a
    /// [`TextureRegistration`](crate::rhi::TextureRegistration).
    ///
    /// LimitedAccess mirror — cdylib dispatch inherits the
    /// `resolve_texture_registration_by_surface_id` slot via
    /// [`Self::inherited_limited_unchecked`].
    pub fn resolve_texture_registration_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<crate::rhi::TextureRegistration> {
        self.inherited_limited_unchecked()
            .resolve_texture_registration_by_surface_id(surface_id, texture_layout, width, height)
    }

    /// Resolve an incoming `VideoFrame`'s `surface_id` directly to a
    /// [`Texture`](crate::rhi::Texture).
    ///
    /// LimitedAccess mirror — cdylib dispatch inherits the
    /// `resolve_texture_by_surface_id` slot via
    /// [`Self::inherited_limited_unchecked`].
    pub fn resolve_texture_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<crate::rhi::Texture> {
        self.inherited_limited_unchecked()
            .resolve_texture_by_surface_id(surface_id, texture_layout, width, height)
    }

    /// Check out a shared surface as a CPU-readable
    /// [`PixelBuffer`](crate::rhi::PixelBuffer) by `surface_id`.
    ///
    /// LimitedAccess mirror — inherits the `check_out_surface` slot via
    /// [`Self::inherited_limited_unchecked`].
    pub fn check_out_surface(&self, surface_id: &str) -> Result<crate::rhi::PixelBuffer> {
        self.inherited_limited_unchecked()
            .check_out_surface(surface_id)
    }

    /// Resolve a `surface_id` to a CPU-readable
    /// [`PixelBuffer`](crate::rhi::PixelBuffer).
    ///
    /// LimitedAccess mirror — inherits the `resolve_pixel_buffer_by_surface_id`
    /// slot via [`Self::inherited_limited_unchecked`].
    pub fn resolve_pixel_buffer_by_surface_id(
        &self,
        surface_id: &str,
    ) -> Result<crate::rhi::PixelBuffer> {
        self.inherited_limited_unchecked()
            .resolve_pixel_buffer_by_surface_id(surface_id)
    }

    /// Acquire a pooled [`PixelBuffer`](crate::rhi::PixelBuffer) for CPU→GPU
    /// upload.
    ///
    /// LimitedAccess mirror — inherits the `acquire_pixel_buffer` slot via
    /// [`Self::inherited_limited_unchecked`].
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(crate::rhi::PixelBufferPoolId, crate::rhi::PixelBuffer)> {
        self.inherited_limited_unchecked()
            .acquire_pixel_buffer(width, height, format)
    }

    /// Look up a pooled [`PixelBuffer`](crate::rhi::PixelBuffer) by its pool id.
    ///
    /// LimitedAccess mirror — inherits the `get_pixel_buffer` slot via
    /// [`Self::inherited_limited_unchecked`].
    pub fn get_pixel_buffer(
        &self,
        pool_id: &crate::rhi::PixelBufferPoolId,
    ) -> Result<crate::rhi::PixelBuffer> {
        self.inherited_limited_unchecked().get_pixel_buffer(pool_id)
    }

    /// Register a texture under a `surface_id` (defaults to layout UNDEFINED).
    ///
    /// LimitedAccess mirror — inherits the `register_texture` slot via
    /// [`Self::inherited_limited_unchecked`].
    pub fn register_texture(&self, id: &str, texture: crate::rhi::Texture) {
        self.inherited_limited_unchecked()
            .register_texture(id, texture);
    }

    /// Register a texture under a `surface_id` with an explicit initial layout.
    ///
    /// LimitedAccess mirror — inherits the `register_texture` slot via
    /// [`Self::inherited_limited_unchecked`].
    pub fn register_texture_with_layout(
        &self,
        id: &str,
        texture: crate::rhi::Texture,
        initial_layout: VulkanLayout,
    ) {
        self.inherited_limited_unchecked()
            .register_texture_with_layout(id, texture, initial_layout);
    }

    /// Unregister a texture by its `surface_id`.
    ///
    /// LimitedAccess mirror — inherits the `unregister_texture` slot via
    /// [`Self::inherited_limited_unchecked`].
    pub fn unregister_texture(&self, id: &str) {
        self.inherited_limited_unchecked().unregister_texture(id);
    }

    /// Acquire a pooled scratch texture as a
    /// [`PooledTextureHandle`](crate::rhi::PooledTextureHandle).
    ///
    /// LimitedAccess mirror — inherits the `acquire_texture` slot via
    /// [`Self::inherited_limited_unchecked`].
    pub fn acquire_texture(
        &self,
        desc: &crate::rhi::TexturePoolDescriptor,
    ) -> Result<crate::rhi::PooledTextureHandle> {
        self.inherited_limited_unchecked().acquire_texture(desc)
    }

    /// Allocate a render-target-capable DMA-BUF VkImage (privileged
    /// host-only adapter primitive). Dispatches through the
    /// [`GpuContextFullAccessVTable`]'s
    /// `acquire_render_target_dma_buf_image` slot.
    pub fn acquire_render_target_dma_buf_image(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<crate::rhi::Texture> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "acquire_render_target_dma_buf_image: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut out_texture: std::mem::MaybeUninit<crate::rhi::Texture> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle (scope token) were paired at
        // construction; the host writes a valid Texture into
        // `out_texture` on success.
        let status = unsafe {
            ((*self.vtable).acquire_render_target_dma_buf_image)(
                self.handle,
                width,
                height,
                format as u32,
                out_texture.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid value.
            Ok(unsafe { out_texture.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Pre-allocate a ring of `count` non-exportable DEVICE_LOCAL
    /// textures and register each in the same-process texture cache.
    /// Dispatches through the [`GpuContextFullAccessVTable`]'s
    /// `create_texture_ring` slot.
    pub fn create_texture_ring(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
        usages: TextureUsages,
        count: usize,
    ) -> Result<crate::rhi::TextureRing> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "create_texture_ring: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut out_ring: *const c_void = std::ptr::null();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle (scope token) paired at construction.
        let status = unsafe {
            ((*self.vtable).create_texture_ring)(
                self.handle,
                width,
                height,
                format as u32,
                usages.bits(),
                count,
                &mut out_ring,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        if out_ring.is_null() {
            return Err(Error::GpuError(
                "create_texture_ring: host signaled success but out_ring is null".into(),
            ));
        }
        // PluginAbiObject: cached POD descriptors come from the caller's
        // own inputs (we know width / height / format / count). The
        // per-type methods vtable comes from `host_callbacks()`.
        let methods_vtable = crate::plugin::host_callbacks()
            .map(|c| c.texture_ring_methods_vtable)
            .unwrap_or(std::ptr::null());
        Ok(crate::rhi::TextureRing {
            handle: out_ring,
            vtable: self.vtable,
            methods_vtable,
            cached_len: count as u32,
            cached_width: width,
            cached_height: height,
            cached_format: format as u32,
        })
    }

    /// Create a single-in-flight GPU→CPU texture readback bound to a
    /// fixed format/extent, returned as the layout-stable
    /// [`crate::rhi::TextureReadback`] PluginAbiObject. Dispatches through
    /// the [`GpuContextFullAccessVTable`]'s `create_texture_readback`
    /// slot; the host rejects planar `Nv12`. For parallel readbacks,
    /// hold N handles.
    pub fn create_texture_readback(
        &self,
        label: &str,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<crate::rhi::TextureReadback> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "create_texture_readback: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut out_readback: *const c_void = std::ptr::null();
        let mut out_handle_id: u64 = 0;
        let mut out_staging_size: u64 = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle (scope token) paired at construction.
        let status = unsafe {
            ((*self.vtable).create_texture_readback)(
                self.handle,
                label.as_ptr(),
                label.len(),
                width,
                height,
                format as u32,
                &mut out_readback,
                &mut out_handle_id as *mut u64,
                &mut out_staging_size as *mut u64,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        if out_readback.is_null() {
            return Err(Error::GpuError(
                "create_texture_readback: host signaled success but out handle is null".into(),
            ));
        }
        // PluginAbiObject: cached POD comes from the host's out-params
        // (handle id + staging size, never recomputed) and the caller's
        // own inputs (width / height / format). The per-type methods
        // vtable comes from `host_callbacks()`.
        let methods_vtable = crate::plugin::host_callbacks()
            .map(|c| c.vulkan_texture_readback_methods_vtable)
            .unwrap_or(std::ptr::null());
        Ok(crate::rhi::TextureReadback {
            handle: out_readback,
            vtable: self.vtable,
            methods_vtable,
            cached_handle_id: out_handle_id,
            cached_staging_size: out_staging_size,
            cached_width: width,
            cached_height: height,
            cached_format_raw: format as u32,
            _reserved_padding: 0,
        })
    }

    /// Mint a hardware video [`EncoderSession`](crate::rhi::EncoderSession)
    /// from the frozen
    /// [`VideoEncoderSessionDescriptorRepr`](streamlib_plugin_abi::VideoEncoderSessionDescriptorRepr)
    /// the caller fills (width / height / fps / codec / preset / rate
    /// control / color VUI). The host constructs the `SimpleEncoder` on its
    /// device and returns the Box-opaque handle plus the codec-aligned
    /// extent (RGBA input to
    /// [`EncoderSession::submit_texture`](crate::rhi::EncoderSession::submit_texture)
    /// must be at least that large). Dispatches through the
    /// [`GpuContextFullAccessVTable`]'s `create_encoder_session` slot; the
    /// host rejects an unsupported codec/preset with a typed error.
    pub fn create_encoder_session(
        &self,
        descriptor: &streamlib_plugin_abi::VideoEncoderSessionDescriptorRepr,
    ) -> Result<crate::rhi::EncoderSession> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "create_encoder_session: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut out_session: *const c_void = std::ptr::null();
        let mut out_aligned_width: u32 = 0;
        let mut out_aligned_height: u32 = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle (scope token) paired at construction;
        // `descriptor` is borrowed (POD) for the duration of the call; the
        // host writes the opaque handle + aligned extent on success.
        let status = unsafe {
            ((*self.vtable).create_encoder_session)(
                self.handle,
                descriptor as *const streamlib_plugin_abi::VideoEncoderSessionDescriptorRepr,
                &mut out_session,
                &mut out_aligned_width as *mut u32,
                &mut out_aligned_height as *mut u32,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        if out_session.is_null() {
            return Err(Error::GpuError(
                "create_encoder_session: host signaled success but out session is null".into(),
            ));
        }
        // The per-type methods vtable comes from `host_callbacks()` (cdylib
        // mode) — no host static exists in the engine-free SDK.
        let methods_vtable = crate::plugin::host_callbacks()
            .map(|c| c.video_encoder_session_methods_vtable)
            .unwrap_or(std::ptr::null());
        Ok(crate::rhi::EncoderSession {
            handle: out_session,
            vtable: self.vtable,
            methods_vtable,
            aligned_width: out_aligned_width,
            aligned_height: out_aligned_height,
        })
    }

    /// Mint a hardware video [`DecoderSession`](crate::rhi::DecoderSession)
    /// from the frozen
    /// [`VideoDecoderSessionDescriptorRepr`](streamlib_plugin_abi::VideoDecoderSessionDescriptorRepr)
    /// the caller fills (codec / optional max resolution / DPB output mode
    /// / RGBA-vs-NV12 output). The host constructs the `SimpleDecoder` on
    /// its device and returns the Box-opaque handle; coded dimensions
    /// auto-detect from the first SPS (query
    /// [`DecoderSession::dimensions`](crate::rhi::DecoderSession::dimensions)
    /// after the first `feed`). Dispatches through the
    /// [`GpuContextFullAccessVTable`]'s `create_decoder_session` slot; the
    /// host rejects an unsupported codec / DPB output mode with a typed
    /// error.
    pub fn create_decoder_session(
        &self,
        descriptor: &streamlib_plugin_abi::VideoDecoderSessionDescriptorRepr,
    ) -> Result<crate::rhi::DecoderSession> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "create_decoder_session: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut out_session: *const c_void = std::ptr::null();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle (scope token) paired at construction;
        // `descriptor` is borrowed (POD) for the duration of the call; the
        // host writes the opaque handle on success.
        let status = unsafe {
            ((*self.vtable).create_decoder_session)(
                self.handle,
                descriptor as *const streamlib_plugin_abi::VideoDecoderSessionDescriptorRepr,
                &mut out_session,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        if out_session.is_null() {
            return Err(Error::GpuError(
                "create_decoder_session: host signaled success but out session is null".into(),
            ));
        }
        // The per-type methods vtable comes from `host_callbacks()` (cdylib
        // mode) — no host static exists in the engine-free SDK.
        let methods_vtable = crate::plugin::host_callbacks()
            .map(|c| c.video_decoder_session_methods_vtable)
            .unwrap_or(std::ptr::null());
        Ok(crate::rhi::DecoderSession {
            handle: out_session,
            vtable: self.vtable,
            methods_vtable,
        })
    }

    /// Obtain the host's cross-process
    /// [`SurfaceStore`](crate::rhi::SurfaceStore) producer handle.
    ///
    /// LimitedAccess mirror — inherits the `surface_store` slot via the
    /// crate-internal `inherited_limited_unchecked` view.
    pub fn surface_store(&self) -> crate::rhi::SurfaceStore {
        self.inherited_limited_unchecked().surface_store()
    }

    /// Construct an OPAQUE_FD-exportable timeline semaphore with the given
    /// `initial_value`. Dispatches through the
    /// [`GpuContextFullAccessVTable`]'s
    /// `create_exportable_timeline_semaphore` slot; the host writes a
    /// fully-initialized [`HostTimelineSemaphore`](crate::rhi::HostTimelineSemaphore)
    /// (handle + host-static methods vtable) into the out-param.
    ///
    /// The returned timeline can `export_opaque_fd` for cross-process /
    /// CUDA surface-share sync, and its inner handle registers into
    /// surface-share via [`crate::rhi::SurfaceStore::register_texture`].
    pub fn create_exportable_timeline_semaphore(
        &self,
        initial_value: u64,
    ) -> Result<crate::rhi::HostTimelineSemaphore> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "create_exportable_timeline_semaphore: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut out_timeline: std::mem::MaybeUninit<crate::rhi::HostTimelineSemaphore> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle (scope token) paired at construction; the
        // host writes a fully-initialized 16-byte HostTimelineSemaphore
        // (handle + host-static methods pointer) into `out_timeline` on
        // success.
        let status = unsafe {
            ((*self.vtable).create_exportable_timeline_semaphore)(
                self.handle,
                initial_value,
                out_timeline.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid value.
            Ok(unsafe { out_timeline.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Acquire a cached `(src, dst)`-keyed color converter. Dispatches
    /// through the [`GpuContextFullAccessVTable`]'s `color_converter`
    /// slot.
    pub fn color_converter(
        &self,
        src: PixelFormat,
        dst: PixelFormat,
    ) -> Result<crate::rhi::RhiColorConverter> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "color_converter: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut out_converter: *const c_void = std::ptr::null();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle (scope token) paired at construction.
        let status = unsafe {
            ((*self.vtable).color_converter)(
                self.handle,
                src as u32,
                dst as u32,
                &mut out_converter,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        if out_converter.is_null() {
            return Err(Error::GpuError(
                "color_converter: host signaled success but out_converter is null".into(),
            ));
        }
        let methods_vtable = crate::plugin::host_callbacks()
            .map(|c| c.rhi_color_converter_methods_vtable)
            .unwrap_or(std::ptr::null());
        Ok(crate::rhi::RhiColorConverter {
            handle: out_converter,
            vtable: self.vtable,
            methods_vtable,
            cached_src_format_raw: src as u32,
            cached_dst_format_raw: dst as u32,
        })
    }

    /// Upload a HOST_VISIBLE [`PixelBuffer`](crate::rhi::PixelBuffer)'s
    /// contents to a freshly-allocated GPU texture and register it under
    /// `surface_id`. Dispatches through the
    /// [`GpuContextFullAccessVTable`]'s `upload_pixel_buffer_as_texture`
    /// slot.
    ///
    /// This is the escalate-privileged FullAccess tier — the host
    /// allocates a new texture per call. For per-frame hot paths, prefer a
    /// setup-time `TextureRing` on the host side rather than repeated
    /// escalations. The `pixel_buffer` crosses the plugin ABI by borrowed
    /// pointer (the PluginAbiObject twin pattern); the host reads it back
    /// through its own RHI `PixelBuffer` mirror.
    ///
    /// The source must be an RGBA8/BGRA8 HOST_VISIBLE buffer of at least
    /// `width * height * 4` bytes; `width` / `height` describe the logical
    /// region copied and are NOT read from the `PixelBuffer`'s cached dims.
    /// The host validates the required byte size against the source buffer's
    /// actual allocation and returns a typed error on overflow (rather than
    /// faulting the device).
    pub fn upload_pixel_buffer_as_texture(
        &self,
        surface_id: &str,
        pixel_buffer: &crate::rhi::PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "upload_pixel_buffer_as_texture: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle (scope token) paired at construction;
        // `pixel_buffer` is a borrowed PluginAbiObject valid for the call.
        // The host reads it back as its own `crate::core::rhi::PixelBuffer`
        // mirror (layout-locked by the pixel_buffer layout test).
        let status = unsafe {
            ((*self.vtable).upload_pixel_buffer_as_texture)(
                self.handle,
                surface_id.as_ptr(),
                surface_id.len(),
                pixel_buffer as *const crate::rhi::PixelBuffer as *const c_void,
                width,
                height,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Create a compute kernel from a SPIR-V shader and a binding
    /// declaration. Dispatches through the [`GpuContextFullAccessVTable`]'s
    /// `create_compute_kernel` slot; the host reflects the SPIR-V,
    /// validates the declared bindings match the shader, and allocates
    /// the Vulkan pipeline host-side.
    pub fn create_compute_kernel(
        &self,
        descriptor: &crate::rhi::ComputeKernelDescriptor<'_>,
    ) -> Result<crate::rhi::VulkanComputeKernel> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "create_compute_kernel: GpuContextFullAccess has null vtable".into(),
            ));
        }
        // Stage the descriptor into its repr + backing bindings_buf; the
        // backing Vec must stay alive for the vtable call because the
        // repr's bindings_ptr borrows into it.
        let (repr, _bindings_buf) = crate::rhi::stage_compute_kernel_descriptor(descriptor);
        let mut out_kernel: *const c_void = std::ptr::null();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle (scope token) paired at construction;
        // `repr` borrows into `_bindings_buf` / `descriptor`, both alive
        // for the duration of the call.
        let status = unsafe {
            ((*self.vtable).create_compute_kernel)(
                self.handle,
                &repr,
                &mut out_kernel,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        if out_kernel.is_null() {
            return Err(Error::GpuError(
                "create_compute_kernel: host signaled success but out_kernel is null".into(),
            ));
        }
        let methods_vtable = crate::plugin::host_callbacks()
            .map(|c| c.vulkan_compute_kernel_methods_vtable)
            .unwrap_or(std::ptr::null());
        Ok(crate::rhi::VulkanComputeKernel {
            handle: out_kernel,
            vtable: self.vtable,
            methods_vtable,
            cached_push_constant_size: descriptor.push_constant_size,
            _reserved_padding: 0,
        })
    }

    /// Create a graphics kernel from a multi-stage SPIR-V set, binding
    /// declaration, and fixed-function pipeline state. Dispatches through
    /// the [`GpuContextFullAccessVTable`]'s `create_graphics_kernel` slot;
    /// the host reflects every stage's SPIR-V, validates the declared
    /// bindings match the shaders, and allocates the Vulkan pipeline
    /// host-side.
    pub fn create_graphics_kernel(
        &self,
        descriptor: &crate::rhi::GraphicsKernelDescriptor<'_>,
    ) -> Result<crate::rhi::VulkanGraphicsKernel> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "create_graphics_kernel: GpuContextFullAccess has null vtable".into(),
            ));
        }
        // Stage the descriptor into its repr + the keepalive backing Vecs;
        // every backing Vec must stay alive for the vtable call because the
        // repr's pointer fields borrow into them.
        let (repr, _stage) = crate::rhi::stage_graphics_kernel_descriptor(descriptor);
        let mut out_kernel: *const c_void = std::ptr::null();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle (scope token) paired at construction;
        // `repr` borrows into `_stage` / `descriptor`, both alive for the
        // duration of the call.
        let status = unsafe {
            ((*self.vtable).create_graphics_kernel)(
                self.handle,
                &repr,
                &mut out_kernel,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        if out_kernel.is_null() {
            return Err(Error::GpuError(
                "create_graphics_kernel: host signaled success but out_kernel is null".into(),
            ));
        }
        let methods_vtable = crate::plugin::host_callbacks()
            .map(|c| c.vulkan_graphics_kernel_methods_vtable)
            .unwrap_or(std::ptr::null());
        Ok(crate::rhi::VulkanGraphicsKernel {
            handle: out_kernel,
            vtable: self.vtable,
            methods_vtable,
            cached_push_constant_size: descriptor.push_constants.size,
            cached_descriptor_sets_in_flight: descriptor.descriptor_sets_in_flight,
        })
    }

    /// Build an engine-owned multi-step command-buffer recorder.
    /// Dispatches through the [`GpuContextFullAccessVTable`]'s
    /// `create_command_recorder` slot.
    pub fn create_command_recorder(&self, label: &str) -> Result<crate::rhi::RhiCommandRecorder> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "create_command_recorder: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut out_recorder: std::mem::MaybeUninit<crate::rhi::RhiCommandRecorder> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle (scope token) paired at construction;
        // the host writes the RhiCommandRecorder by value (layout
        // byte-identical via `#[repr(C)]`), populating both `vtable` and
        // `methods_vtable` with host-static addresses.
        let status = unsafe {
            ((*self.vtable).create_command_recorder)(
                self.handle,
                label.as_ptr(),
                label.len(),
                out_recorder.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid value.
            Ok(unsafe { out_recorder.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Build a swapchain-backed [`PresentTarget`](crate::rhi::PresentTarget)
    /// from a native `window` handle. `window` is a flattened
    /// [`RawWindowHandleRepr`](streamlib_plugin_abi::RawWindowHandleRepr)
    /// the caller projects from its own windowing toolkit (winit lives in
    /// the display package — window ownership is host-portable, never baked
    /// into the ABI). `color` `None` = legacy SDR pick. The window must
    /// outlive the returned target; the host owns the `VkSurfaceKHR` from
    /// creation. Dispatches through the [`GpuContextFullAccessVTable`]'s
    /// `create_present_target` slot.
    pub fn create_present_target(
        &self,
        window: &streamlib_plugin_abi::RawWindowHandleRepr,
        width: u32,
        height: u32,
        vsync: bool,
        color: Option<streamlib_plugin_abi::ColorTraitsRepr>,
    ) -> Result<crate::rhi::PresentTarget> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "create_present_target: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let color_ptr = color
            .as_ref()
            .map(|c| c as *const streamlib_plugin_abi::ColorTraitsRepr)
            .unwrap_or(std::ptr::null());
        let mut out_present: std::mem::MaybeUninit<crate::rhi::PresentTarget> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle (scope token) paired at construction; the
        // host writes the PresentTarget by value (`#[repr(C)]`
        // byte-identical), populating `vtable` + `methods_vtable` with
        // host-static addresses and caching the swapchain color format.
        let status = unsafe {
            ((*self.vtable).create_present_target)(
                self.handle,
                window as *const streamlib_plugin_abi::RawWindowHandleRepr,
                width,
                height,
                vsync as u32,
                color_ptr,
                out_present.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid value.
            Ok(unsafe { out_present.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Allocate an OPAQUE_FD-exportable `VkBuffer` as a
    /// [`StorageBuffer`](crate::rhi::StorageBuffer) (`device_local = true`
    /// → VRAM-resident CUDA-visible; `false` → HOST_VISIBLE). The
    /// cdylib-safe OPAQUE_FD/CUDA producer allocation (#1262); dispatches
    /// through the [`GpuContextFullAccessVTable`]'s
    /// `create_opaque_fd_export_buffer` slot.
    pub fn create_opaque_fd_export_buffer(
        &self,
        byte_size: u64,
        device_local: bool,
    ) -> Result<crate::rhi::StorageBuffer> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "create_opaque_fd_export_buffer: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut out: std::mem::MaybeUninit<crate::rhi::StorageBuffer> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle (scope token) paired at construction;
        // the host writes a valid `StorageBuffer` PluginAbiObject (with a
        // populated `byte_size_cached`) into `out` on success.
        let status = unsafe {
            ((*self.vtable).create_opaque_fd_export_buffer)(
                self.handle,
                byte_size,
                u8::from(device_local),
                out.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid value.
            Ok(unsafe { out.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Export a fresh dup'd OPAQUE_FD plus byte size and exporting-device
    /// UUID from `buffer`. The fd transfers to the caller (import it once
    /// via `cudaImportExternalMemory`); the 16-byte UUID binds the CUDA
    /// context to the matching physical device. Dispatches through the
    /// `export_storage_buffer_opaque_fd` slot (#1262).
    pub fn export_storage_buffer_opaque_fd(
        &self,
        buffer: &crate::rhi::StorageBuffer,
    ) -> Result<(std::os::unix::io::RawFd, u64, [u8; 16])> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "export_storage_buffer_opaque_fd: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut descriptor = crate::rhi::OpaqueFdExportDescriptorRepr::default();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle paired at construction; `buffer` is a
        // borrowed StorageBuffer valid for the call; the host writes the
        // descriptor (or leaves `fd = -1` on refusal) into `descriptor`.
        let status = unsafe {
            ((*self.vtable).export_storage_buffer_opaque_fd)(
                self.handle,
                buffer as *const _ as *const c_void,
                &mut descriptor,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok((descriptor.fd, descriptor.size, descriptor.device_uuid))
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Wrap an existing OPAQUE_FD [`StorageBuffer`](crate::rhi::StorageBuffer)
    /// as a [`PixelBuffer`](crate::rhi::PixelBuffer) sharing the same
    /// allocation, so the flat CUDA buffer can register through the
    /// surface-store `register_pixel_buffer_with_timeline` path (#1262).
    /// Dispatches through the `wrap_storage_buffer_as_pixel_buffer` slot.
    pub fn wrap_storage_buffer_as_pixel_buffer(
        &self,
        storage_buffer: &crate::rhi::StorageBuffer,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: PixelFormat,
    ) -> Result<crate::rhi::PixelBuffer> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "wrap_storage_buffer_as_pixel_buffer: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut out: std::mem::MaybeUninit<crate::rhi::PixelBuffer> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle paired at construction; `storage_buffer`
        // borrowed for the call; the host writes a valid `PixelBuffer`
        // PluginAbiObject (with cached width/height/format) into `out`.
        let status = unsafe {
            ((*self.vtable).wrap_storage_buffer_as_pixel_buffer)(
                self.handle,
                storage_buffer as *const _ as *const c_void,
                width,
                height,
                bytes_per_pixel,
                format as u32,
                out.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid value.
            Ok(unsafe { out.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Per-frame CUDA producer copy: `vkCmdCopyImageToBuffer` from
    /// `source_texture` (currently in `source_layout`) into `dst` in one
    /// host-device submission (#1262). Dispatches through the
    /// `copy_texture_to_storage_buffer_and_signal` slot.
    ///
    /// In the same submission the host GPU-waits on `consume_done` at its
    /// value before the copy and signals `produce_done` at its value on
    /// completion (single-writer-per-edge). Pass `None` for either edge to
    /// skip that wait / signal. A cross-process consumer MUST be handed a
    /// `produce_done` timeline here — the GPU-queue completion this method
    /// schedules is what advances it; a fully-`None` call blocks host-side
    /// until the copy completes with no cross-API sync, so a subprocess
    /// consumer waiting on a never-signalled `produce_done` would block
    /// forever.
    pub fn copy_texture_to_storage_buffer_and_signal(
        &self,
        source_texture: &crate::rhi::Texture,
        source_layout: VulkanLayout,
        dst: &crate::rhi::StorageBuffer,
        consume_done: Option<(&crate::rhi::HostTimelineSemaphore, u64)>,
        produce_done: Option<(&crate::rhi::HostTimelineSemaphore, u64)>,
    ) -> Result<()> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "copy_texture_to_storage_buffer_and_signal: GpuContextFullAccess has null vtable"
                    .into(),
            ));
        }
        let (consume_handle, consume_value) = match consume_done {
            Some((timeline, value)) => (timeline.cdylib_handle(), value),
            None => (std::ptr::null(), 0),
        };
        let (produce_handle, produce_value) = match produce_done {
            Some((timeline, value)) => (timeline.cdylib_handle(), value),
            None => (std::ptr::null(), 0),
        };
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle paired at construction; the texture +
        // storage buffer are borrowed for the call; each timeline handle is
        // the host-minted `Arc::into_raw(Arc<HostVulkanTimelineSemaphore>)`
        // inner pointer the host derefs as a borrow (null selects "no wait /
        // no signal" on that edge).
        let status = unsafe {
            ((*self.vtable).copy_texture_to_storage_buffer_and_signal)(
                self.handle,
                source_texture.handle,
                source_layout.0,
                dst as *const _ as *const c_void,
                consume_handle,
                consume_value,
                produce_handle,
                produce_value,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Read the host GPU capability snapshot (v5 `gpu_capabilities` slot):
    /// device name plus external-memory / cross-device-DMA-BUF-probe /
    /// ray-tracing capability bools, read once at setup for device-vendor
    /// branching. Dispatches through the [`GpuContextFullAccessVTable`]'s
    /// `gpu_capabilities` slot.
    pub fn gpu_capabilities(&self) -> Result<GpuCapabilities> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "gpu_capabilities: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut caps: std::mem::MaybeUninit<GpuCapabilitiesRepr> = std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle paired at construction; the host fully
        // populates `caps` (fixed-size device_name buffer + len + capability
        // bools) on success.
        let status = unsafe {
            ((*self.vtable).gpu_capabilities)(
                self.handle,
                caps.as_mut_ptr(),
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        // SAFETY: host signaled success and wrote a valid GpuCapabilitiesRepr.
        let caps = unsafe { caps.assume_init() };
        Ok(gpu_capabilities_from_repr(&caps))
    }

    /// Import a V4L2 (or otherwise externally-allocated) DMA-BUF FD as a
    /// [`StorageBuffer`](crate::rhi::StorageBuffer) (SSBO-shaped) over the
    /// v7 slot. Dispatches through the [`GpuContextFullAccessVTable`]'s
    /// `import_dma_buf_storage_buffer` slot.
    ///
    /// **The host consumes `fd` on success** (`vkImportMemoryFdInfoKHR`
    /// takes ownership). On failure the caller retains ownership and must
    /// close it.
    pub fn import_dma_buf_storage_buffer(
        &self,
        fd: i32,
        byte_size: u64,
    ) -> Result<crate::rhi::StorageBuffer> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "import_dma_buf_storage_buffer: GpuContextFullAccess has null vtable".into(),
            ));
        }
        let mut out: std::mem::MaybeUninit<crate::rhi::StorageBuffer> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle paired at construction; the host writes a
        // valid `StorageBuffer` PluginAbiObject (with populated
        // `byte_size_cached`) into `out` and consumes `fd` on success.
        let status = unsafe {
            ((*self.vtable).import_dma_buf_storage_buffer)(
                self.handle,
                fd,
                byte_size,
                out.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid value.
            Ok(unsafe { out.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }
}

// =============================================================================
// RuntimeContext identifier trampolines (shared by both cdylib-arm shims)
// =============================================================================

/// Turn the ABI's `(out_buf, cap, out_len) -> usize` byte-copy callback for
/// [`RuntimeContextVTable::runtime_id_copy`] into an owned `String`. Calls
/// the callback with a stack scratch buffer first, then grows and retries
/// once when the host reports a required length exceeding the scratch cap.
///
/// # Safety
///
/// `vtable` must point at a valid [`RuntimeContextVTable`] paired with
/// `handle`; the `runtime_id_copy` callback writes UTF-8 bytes.
unsafe fn vtable_copy_runtime_id(
    handle: *const c_void,
    vtable: *const RuntimeContextVTable,
) -> String {
    // Most runtime ids are short (~26 bytes for cuid2 plus the "R" prefix);
    // 64 bytes covers every reasonable id without a retry.
    let mut scratch = [0u8; 64];
    let mut written: usize = 0;
    let required = unsafe {
        ((*vtable).runtime_id_copy)(handle, scratch.as_mut_ptr(), scratch.len(), &mut written)
    };
    if required <= scratch.len() {
        // Bound the host-reported `written` to the scratch capacity: a
        // misbehaving host reporting written > 64 must not index past the
        // stack buffer (defense-in-depth at the host-trust ABI boundary).
        let copied = written.min(scratch.len());
        // SAFETY: the callback contractually writes UTF-8 bytes.
        unsafe { String::from_utf8_unchecked(scratch[..copied].to_vec()) }
    } else {
        let mut buf = vec![0u8; required];
        let mut written2: usize = 0;
        unsafe {
            ((*vtable).runtime_id_copy)(handle, buf.as_mut_ptr(), buf.len(), &mut written2);
        }
        buf.truncate(written2);
        // SAFETY: the callback contractually writes UTF-8 bytes.
        unsafe { String::from_utf8_unchecked(buf) }
    }
}

/// Turn the ABI's [`RuntimeContextVTable::processor_id_copy`] callback into
/// an owned `Option<String>` — `None` when the host returns `-1` (the
/// shared/global context has no processor id). Same grow-and-retry as
/// [`vtable_copy_runtime_id`].
///
/// # Safety
///
/// See [`vtable_copy_runtime_id`].
unsafe fn vtable_copy_processor_id(
    handle: *const c_void,
    vtable: *const RuntimeContextVTable,
) -> Option<String> {
    let mut scratch = [0u8; 64];
    let mut written: usize = 0;
    let required = unsafe {
        ((*vtable).processor_id_copy)(handle, scratch.as_mut_ptr(), scratch.len(), &mut written)
    };
    if required < 0 {
        return None;
    }
    let required = required as usize;
    if required <= scratch.len() {
        // Bound the host-reported `written` to the scratch capacity: a
        // misbehaving host reporting written > 64 must not index past the
        // stack buffer (defense-in-depth at the host-trust ABI boundary).
        let copied = written.min(scratch.len());
        // SAFETY: the callback contractually writes UTF-8 bytes.
        Some(unsafe { String::from_utf8_unchecked(scratch[..copied].to_vec()) })
    } else {
        let mut buf = vec![0u8; required];
        let mut written2: usize = 0;
        unsafe {
            ((*vtable).processor_id_copy)(handle, buf.as_mut_ptr(), buf.len(), &mut written2);
        }
        buf.truncate(written2);
        // SAFETY: the callback contractually writes UTF-8 bytes.
        Some(unsafe { String::from_utf8_unchecked(buf) })
    }
}

// =============================================================================
// RuntimeContextFullAccess — cdylib arm
// =============================================================================

/// Privileged-`RuntimeContext` view passed to `setup` / `teardown` /
/// Manual-mode `start` / `stop`. Cdylib-arm twin of the engine's
/// `RuntimeContextFullAccess`.
///
/// Deliberately `!Clone` and borrow-scoped.
///
/// ```compile_fail
/// fn assert_not_clone<T: Clone>() {}
/// assert_not_clone::<streamlib_plugin_sdk::sdk::context::RuntimeContextFullAccess<'static>>();
/// ```
#[repr(C)]
pub struct RuntimeContextFullAccess<'a> {
    /// Opaque pointer to the host-owned `RuntimeContext`.
    handle: *const c_void,
    /// Pointer to the host's [`RuntimeContextVTable`].
    vtable: *const RuntimeContextVTable,
    gpu_full: GpuContextFullAccess,
    gpu_limited: GpuContextLimitedAccess,
    _marker: PhantomData<&'a ()>,
}

// SAFETY: same shape as the engine twin; every field is an opaque
// pointer / `Send + Sync` embedded view. The host builds the value and
// keeps the backing alive for the borrow's lifetime.
unsafe impl Send for RuntimeContextFullAccess<'_> {}
unsafe impl Sync for RuntimeContextFullAccess<'_> {}

impl<'a> RuntimeContextFullAccess<'a> {
    /// Privileged GPU capability — allocations, device-wide ops, escalate.
    pub fn gpu_full_access(&self) -> &GpuContextFullAccess {
        &self.gpu_full
    }

    /// Restricted GPU capability. Cloneable — hand to a Manual-mode worker
    /// thread during `start()` so it can participate in the hot path with
    /// limited-access operations only.
    pub fn gpu_limited_access(&self) -> &GpuContextLimitedAccess {
        &self.gpu_limited
    }

    /// Runtime unique id as an owned [`String`]. Routed through the
    /// [`RuntimeContextVTable::runtime_id_copy`] slot.
    pub fn runtime_id(&self) -> String {
        // SAFETY: `handle` + `vtable` were paired by the host when it built
        // this view; the `runtime_id_copy` slot writes UTF-8 bytes.
        unsafe { vtable_copy_runtime_id(self.handle, self.vtable) }
    }

    /// Processor unique id as an owned [`String`], or `None` for the
    /// shared/global context. Routed through
    /// [`RuntimeContextVTable::processor_id_copy`].
    pub fn processor_id(&self) -> Option<String> {
        // SAFETY: see [`Self::runtime_id`].
        unsafe { vtable_copy_processor_id(self.handle, self.vtable) }
    }

    /// Whether this processor is currently paused. Routed through
    /// [`RuntimeContextVTable::is_paused`].
    pub fn is_paused(&self) -> bool {
        // SAFETY: `handle` + `vtable` were paired by the host at construction.
        unsafe { ((*self.vtable).is_paused)(self.handle) }
    }

    /// Whether processing should proceed (not paused). Routed through
    /// [`RuntimeContextVTable::should_process`].
    pub fn should_process(&self) -> bool {
        // SAFETY: `handle` + `vtable` were paired by the host at construction.
        unsafe { ((*self.vtable).should_process)(self.handle) }
    }

    /// Host-owned audio clock as a typed plugin ABI shim. Backed by the
    /// per-RuntimeContext audio-clock handle from
    /// [`RuntimeContextVTable::audio_clock_handle`] paired with the host's
    /// [`AudioClockVTable`](streamlib_plugin_abi::AudioClockVTable) cached
    /// on `HostServices`. Borrow-scoped to the ctx; the returned shim
    /// cannot outlive the lifecycle call.
    pub fn audio_clock(&self) -> AudioClockShim<'a> {
        // SAFETY: `handle` + `vtable` were paired by the host when it
        // built this view; `audio_clock_handle` returns a host-owned
        // handle valid for the runtime's lifetime (outlives this borrow).
        let handle = unsafe { ((*self.vtable).audio_clock_handle)(self.handle) };
        let vtable = crate::plugin::host_callbacks()
            .map(|callbacks| callbacks.audio_clock_vtable)
            .unwrap_or(std::ptr::null());
        AudioClockShim::from_ffi(handle, vtable)
    }
}

// =============================================================================
// RuntimeContextLimitedAccess — cdylib arm
// =============================================================================

/// Restricted-`RuntimeContext` view passed to `process` / `on_pause` /
/// `on_resume`. Cdylib-arm twin of the engine's
/// `RuntimeContextLimitedAccess`.
///
/// Deliberately `!Clone` and borrow-scoped. `gpu_full_access()` is
/// intentionally absent — a `process()` body cannot reach privileged GPU
/// operations.
///
/// ```compile_fail
/// fn assert_not_clone<T: Clone>() {}
/// assert_not_clone::<streamlib_plugin_sdk::sdk::context::RuntimeContextLimitedAccess<'static>>();
/// ```
///
/// ```compile_fail
/// fn reach_full(ctx: &streamlib_plugin_sdk::sdk::context::RuntimeContextLimitedAccess<'_>) {
///     let _ = ctx.gpu_full_access();
/// }
/// ```
#[repr(C)]
pub struct RuntimeContextLimitedAccess<'a> {
    handle: *const c_void,
    vtable: *const RuntimeContextVTable,
    gpu_limited: GpuContextLimitedAccess,
    _marker: PhantomData<&'a ()>,
}

// SAFETY: see [`RuntimeContextFullAccess`].
unsafe impl Send for RuntimeContextLimitedAccess<'_> {}
unsafe impl Sync for RuntimeContextLimitedAccess<'_> {}

impl<'a> RuntimeContextLimitedAccess<'a> {
    /// Restricted GPU capability — cheap, pool-backed, non-allocating ops.
    pub fn gpu_limited_access(&self) -> &GpuContextLimitedAccess {
        &self.gpu_limited
    }

    /// Runtime unique id as an owned [`String`]. See
    /// [`RuntimeContextFullAccess::runtime_id`].
    pub fn runtime_id(&self) -> String {
        // SAFETY: see [`RuntimeContextFullAccess::runtime_id`].
        unsafe { vtable_copy_runtime_id(self.handle, self.vtable) }
    }

    /// Processor unique id as an owned [`String`], or `None` for the
    /// shared/global context. See
    /// [`RuntimeContextFullAccess::processor_id`].
    pub fn processor_id(&self) -> Option<String> {
        // SAFETY: see [`RuntimeContextFullAccess::runtime_id`].
        unsafe { vtable_copy_processor_id(self.handle, self.vtable) }
    }

    /// Whether this processor is currently paused. See
    /// [`RuntimeContextFullAccess::is_paused`]. Available on the restricted
    /// view so a `process()` body can early-out while paused.
    pub fn is_paused(&self) -> bool {
        // SAFETY: `handle` + `vtable` were paired by the host at construction.
        unsafe { ((*self.vtable).is_paused)(self.handle) }
    }

    /// Whether processing should proceed (not paused). See
    /// [`RuntimeContextFullAccess::should_process`].
    pub fn should_process(&self) -> bool {
        // SAFETY: `handle` + `vtable` were paired by the host at construction.
        unsafe { ((*self.vtable).should_process)(self.handle) }
    }

    /// Host-owned audio clock as a typed plugin ABI shim. See
    /// [`RuntimeContextFullAccess::audio_clock`]. Available on the
    /// restricted view so a `process()` body can read tick timing.
    pub fn audio_clock(&self) -> AudioClockShim<'a> {
        // SAFETY: see [`RuntimeContextFullAccess::audio_clock`].
        let handle = unsafe { ((*self.vtable).audio_clock_handle)(self.handle) };
        let vtable = crate::plugin::host_callbacks()
            .map(|callbacks| callbacks.audio_clock_vtable)
            .unwrap_or(std::ptr::null());
        AudioClockShim::from_ffi(handle, vtable)
    }
}

// =============================================================================
// Cross-crate layout lock
// =============================================================================
//
// These views cross the plugin ABI by raw-pointer cast between the host
// build and a separately-built plugin. They are `#[repr(C)]` so the
// layout is identical across builds; these assertions pin the byte shape
// to the SAME numbers the engine asserts in
// `core/context/runtime_context.rs`. A field added to one side but not the
// other trips a test rather than corrupting field reads at runtime.
#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn gpu_context_view_sizes_are_pinned() {
        assert_eq!(size_of::<GpuContextFullAccess>(), 40);
        assert_eq!(align_of::<GpuContextFullAccess>(), 8);
        assert_eq!(size_of::<GpuContextLimitedAccess>(), 16);
        assert_eq!(align_of::<GpuContextLimitedAccess>(), 8);
    }

    #[test]
    fn runtime_context_full_access_layout() {
        assert_eq!(size_of::<RuntimeContextFullAccess<'static>>(), 72);
        assert_eq!(align_of::<RuntimeContextFullAccess<'static>>(), 8);
        assert_eq!(offset_of!(RuntimeContextFullAccess<'static>, handle), 0);
        assert_eq!(offset_of!(RuntimeContextFullAccess<'static>, vtable), 8);
        assert_eq!(offset_of!(RuntimeContextFullAccess<'static>, gpu_full), 16);
        assert_eq!(
            offset_of!(RuntimeContextFullAccess<'static>, gpu_limited),
            56
        );
    }

    #[test]
    fn runtime_context_limited_access_layout() {
        assert_eq!(size_of::<RuntimeContextLimitedAccess<'static>>(), 32);
        assert_eq!(align_of::<RuntimeContextLimitedAccess<'static>>(), 8);
        assert_eq!(offset_of!(RuntimeContextLimitedAccess<'static>, handle), 0);
        assert_eq!(offset_of!(RuntimeContextLimitedAccess<'static>, vtable), 8);
        assert_eq!(
            offset_of!(RuntimeContextLimitedAccess<'static>, gpu_limited),
            16
        );
    }
}

// =============================================================================
// RuntimeContext identifier-accessor dispatch tests (GPU-free)
// =============================================================================
//
// A host-built stub `RuntimeContextVTable` locks the SDK-arm cdylib dispatch
// of `runtime_id()` / `processor_id()` through the shim views: the
// grow-and-retry path when the host reports a required length exceeding the
// 64-byte scratch buffer, and the `-1` → `None` processor-id path. Mental-
// revert the retry arm of `vtable_copy_runtime_id` and the long-id assertion
// truncates at 64 bytes; mental-revert the `required < 0` arm of
// `vtable_copy_processor_id` and the None assertion panics on a bogus
// `String::from_utf8_unchecked`.
#[cfg(test)]
mod runtime_context_id_dispatch_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Deliberately longer than the 64-byte stack scratch buffer so the copy
    // helpers must grow and re-call the slot.
    const LONG_RUNTIME_ID: &[u8] =
        b"R-deliberately-long-runtime-identifier-that-exceeds-the-sixty-four-byte-scratch-buffer";
    const LONG_PROCESSOR_ID: &[u8] =
        b"P-deliberately-long-processor-identifier-that-exceeds-the-sixty-four-byte-scratch-buffer";

    // Comfortably under the 64-byte scratch buffer so the host writes the whole
    // id on the FIRST call and the copy helper returns without growing.
    const SHORT_RUNTIME_ID: &[u8] = b"R-short-runtime-id";
    const SHORT_PROCESSOR_ID: &[u8] = b"P-short-processor-id";

    // Slot-call counters: the first-call no-retry branch must invoke the slot
    // exactly once for a short id.
    static SHORT_RUNTIME_ID_SLOT_CALLS: AtomicUsize = AtomicUsize::new(0);
    static SHORT_PROCESSOR_ID_SLOT_CALLS: AtomicUsize = AtomicUsize::new(0);

    // # Safety: `out_buf` (when non-null) has `cap` writable bytes; `out_len`
    // is a valid `*mut usize`.
    unsafe fn copy_id_into(id: &[u8], out_buf: *mut u8, cap: usize, out_len: *mut usize) -> usize {
        let n = id.len().min(cap);
        if !out_buf.is_null() && n > 0 {
            unsafe { std::ptr::copy_nonoverlapping(id.as_ptr(), out_buf, n) };
        }
        unsafe { *out_len = n };
        id.len()
    }

    unsafe extern "C" fn stub_runtime_id_copy_long(
        _ctx: *const c_void,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> usize {
        unsafe { copy_id_into(LONG_RUNTIME_ID, out_buf, cap, out_len) }
    }

    unsafe extern "C" fn stub_runtime_id_copy_short(
        _ctx: *const c_void,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> usize {
        SHORT_RUNTIME_ID_SLOT_CALLS.fetch_add(1, Ordering::SeqCst);
        unsafe { copy_id_into(SHORT_RUNTIME_ID, out_buf, cap, out_len) }
    }

    unsafe extern "C" fn stub_processor_id_copy_none(
        _ctx: *const c_void,
        _out_buf: *mut u8,
        _cap: usize,
        out_len: *mut usize,
    ) -> isize {
        unsafe { *out_len = 0 };
        -1
    }

    unsafe extern "C" fn stub_processor_id_copy_some_long(
        _ctx: *const c_void,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> isize {
        unsafe { copy_id_into(LONG_PROCESSOR_ID, out_buf, cap, out_len) as isize }
    }

    unsafe extern "C" fn stub_processor_id_copy_some_short(
        _ctx: *const c_void,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> isize {
        SHORT_PROCESSOR_ID_SLOT_CALLS.fetch_add(1, Ordering::SeqCst);
        unsafe { copy_id_into(SHORT_PROCESSOR_ID, out_buf, cap, out_len) as isize }
    }

    unsafe extern "C" fn stub_is_paused(_ctx: *const c_void) -> bool {
        true
    }

    unsafe extern "C" fn stub_should_process(_ctx: *const c_void) -> bool {
        false
    }

    unsafe extern "C" fn stub_opaque_handle(_ctx: *const c_void) -> *const c_void {
        std::ptr::null()
    }

    fn stub_vtable(
        runtime_id_copy: unsafe extern "C" fn(*const c_void, *mut u8, usize, *mut usize) -> usize,
        processor_id_copy: unsafe extern "C" fn(
            *const c_void,
            *mut u8,
            usize,
            *mut usize,
        ) -> isize,
    ) -> RuntimeContextVTable {
        RuntimeContextVTable {
            layout_version: streamlib_plugin_abi::RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION,
            _reserved_padding: 0,
            runtime_id_copy,
            processor_id_copy,
            is_paused: stub_is_paused,
            should_process: stub_should_process,
            gpu_full_access: stub_opaque_handle,
            gpu_limited_access: stub_opaque_handle,
            audio_clock_handle: stub_opaque_handle,
            runtime_ops_handle: stub_opaque_handle,
        }
    }

    fn null_gpu_limited() -> GpuContextLimitedAccess {
        GpuContextLimitedAccess {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
        }
    }

    fn null_gpu_full() -> GpuContextFullAccess {
        GpuContextFullAccess {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
            handle_kind: HandleKind::ScopeToken,
            inherited_lim_handle: std::ptr::null(),
            inherited_lim_vtable: std::ptr::null(),
        }
    }

    #[test]
    fn full_access_runtime_id_survives_grow_and_retry() {
        let vtable = stub_vtable(stub_runtime_id_copy_long, stub_processor_id_copy_none);
        let full = RuntimeContextFullAccess {
            handle: std::ptr::null(),
            vtable: &vtable as *const RuntimeContextVTable,
            gpu_full: null_gpu_full(),
            gpu_limited: null_gpu_limited(),
            _marker: PhantomData,
        };
        assert_eq!(
            full.runtime_id().as_bytes(),
            LONG_RUNTIME_ID,
            "the >64B runtime id must round-trip fully via the grow-and-retry copy"
        );
        assert_eq!(full.processor_id(), None);
        assert!(full.is_paused());
        assert!(!full.should_process());
    }

    #[test]
    fn limited_access_runtime_id_and_processor_id_dispatch() {
        let vtable = stub_vtable(stub_runtime_id_copy_long, stub_processor_id_copy_some_long);
        let limited = RuntimeContextLimitedAccess {
            handle: std::ptr::null(),
            vtable: &vtable as *const RuntimeContextVTable,
            gpu_limited: null_gpu_limited(),
            _marker: PhantomData,
        };
        assert_eq!(limited.runtime_id().as_bytes(), LONG_RUNTIME_ID);
        assert_eq!(
            limited.processor_id().as_deref().map(str::as_bytes),
            Some(LONG_PROCESSOR_ID),
            "a Some processor id longer than 64B must round-trip fully"
        );
        assert!(limited.is_paused());
        assert!(!limited.should_process());
    }

    #[test]
    fn limited_access_processor_id_none_sentinel_returns_none() {
        let vtable = stub_vtable(stub_runtime_id_copy_long, stub_processor_id_copy_none);
        let limited = RuntimeContextLimitedAccess {
            handle: std::ptr::null(),
            vtable: &vtable as *const RuntimeContextVTable,
            gpu_limited: null_gpu_limited(),
            _marker: PhantomData,
        };
        assert_eq!(
            limited.processor_id(),
            None,
            "the -1 sentinel must map to None, never a bogus empty String"
        );
    }

    #[test]
    fn limited_access_short_id_returns_on_first_call_without_retry() {
        SHORT_RUNTIME_ID_SLOT_CALLS.store(0, Ordering::SeqCst);
        SHORT_PROCESSOR_ID_SLOT_CALLS.store(0, Ordering::SeqCst);
        let vtable = stub_vtable(stub_runtime_id_copy_short, stub_processor_id_copy_some_short);
        let limited = RuntimeContextLimitedAccess {
            handle: std::ptr::null(),
            vtable: &vtable as *const RuntimeContextVTable,
            gpu_limited: null_gpu_limited(),
            _marker: PhantomData,
        };
        assert_eq!(limited.runtime_id().as_bytes(), SHORT_RUNTIME_ID);
        assert_eq!(
            limited.processor_id().as_deref().map(str::as_bytes),
            Some(SHORT_PROCESSOR_ID),
        );
        // The whole id fits the 64B scratch on the first call, so the no-retry
        // branch must return without a grow/re-alloc/re-call. Mental-revert the
        // `required <= scratch.len()` short-circuit and each slot is invoked a
        // second time, tripping these counts.
        assert_eq!(
            SHORT_RUNTIME_ID_SLOT_CALLS.load(Ordering::SeqCst),
            1,
            "a short runtime id must return on the first slot call, no grow-and-retry"
        );
        assert_eq!(
            SHORT_PROCESSOR_ID_SLOT_CALLS.load(Ordering::SeqCst),
            1,
            "a short processor id must return on the first slot call, no grow-and-retry"
        );
    }
}

// =============================================================================
// escalate — GPU-free wrapper tests + hardware-gated round-trip
// =============================================================================
//
// The begin/catch/end sequencing itself is a near-verbatim port of the engine
// twin `GpuContextLimitedAccess::escalate_via_vtable`, whose host side (gate
// release, stale-token idempotency, post-`escalate_end` `InvalidEscalateScope`
// backstop) is locked by the engine's `gpu_lim_escalate_vtable_tests`. These
// SDK-arm tests lock the wrapper's GPU-free guard arms (typed errors, no UB,
// closure never runs on a guard path) and compile-lock the privileged
// happy path for /verify-live.
#[cfg(all(test, target_os = "linux"))]
mod escalate_tests {
    use super::*;

    #[test]
    fn full_access_method_on_null_vtable_returns_typed_error_not_ub() {
        // A ScopeToken FullAccess whose vtable is null (a stale / never-armed
        // view) must return a typed error from every method, never
        // dereference the null vtable. Mental-revert the
        // `self.vtable.is_null()` guard in `create_texture_readback` and this
        // segfaults instead of returning `Err`.
        let full = GpuContextFullAccess {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
            handle_kind: HandleKind::ScopeToken,
            inherited_lim_handle: std::ptr::null(),
            inherited_lim_vtable: std::ptr::null(),
        };
        let result = full.create_texture_readback("tap", 16, 16, TextureFormat::Rgba8Unorm);
        assert!(
            matches!(result, Err(Error::GpuError(_))),
            "null-vtable FullAccess call must return a typed GpuError, got {result:?}"
        );
        // `full.handle` is null → Drop returns early; no vtable touched.
    }

    #[test]
    fn upload_pixel_buffer_as_texture_on_null_vtable_returns_typed_error_not_ub() {
        // A ScopeToken FullAccess whose vtable is null must return a typed
        // error from `upload_pixel_buffer_as_texture`, never dereference the
        // null vtable to reach its `upload_pixel_buffer_as_texture` slot.
        // Mental-revert the `self.vtable.is_null()` guard in that method and
        // this segfaults instead of returning `Err`.
        let full = GpuContextFullAccess {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
            handle_kind: HandleKind::ScopeToken,
            inherited_lim_handle: std::ptr::null(),
            inherited_lim_vtable: std::ptr::null(),
        };
        // A null-handle/null-vtable PixelBuffer: its Drop is a no-op (guarded
        // on non-null handle+vtable), so it never touches a vtable slot.
        let pixel_buffer = crate::rhi::PixelBuffer {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
            width: 16,
            height: 16,
            format_raw: 0,
            plane_count_cached: 1,
        };
        let result = full.upload_pixel_buffer_as_texture("tap", &pixel_buffer, 16, 16);
        assert!(
            matches!(result, Err(Error::GpuError(_))),
            "null-vtable upload_pixel_buffer_as_texture must return a typed GpuError, got {result:?}"
        );
        // `full.handle` is null → Drop returns early; no vtable touched.
    }

    #[test]
    fn escalate_with_null_handle_returns_typed_error_and_skips_closure() {
        let limited = GpuContextLimitedAccess {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
        };
        let closure_ran = std::cell::Cell::new(false);
        let result = limited.escalate(|_full| {
            closure_ran.set(true);
            7u32
        });
        assert!(
            matches!(result, Err(Error::GpuError(_))),
            "null handle/vtable must return a typed GpuError, got {result:?}"
        );
        assert!(
            !closure_ran.get(),
            "closure must not run on the null-handle guard path"
        );
        // `limited.handle` is null → Drop skips `drop_handle`; no vtable touched.
    }

    #[test]
    fn escalate_without_host_full_access_vtable_returns_typed_error_before_touching_vtable() {
        // The GPU-free test binary installs no HostCallbacks, so the
        // FullAccess vtable is absent. `escalate` must return a typed error
        // at the full-access-vtable null-check, which runs BEFORE it
        // dereferences the LimitedAccess vtable or calls `escalate_begin`.
        // The fake, never-dereferenced non-null handle/vtable prove that
        // ordering: were the null-check moved after `&*self.vtable` /
        // `escalate_begin`, this would deref the fake vtable and crash.
        // `ManuallyDrop` keeps Drop from touching the fake vtable.
        let sentinel: u8 = 0;
        let limited = std::mem::ManuallyDrop::new(GpuContextLimitedAccess {
            handle: &sentinel as *const u8 as *const c_void,
            vtable: &sentinel as *const u8 as *const GpuContextLimitedAccessVTable,
        });
        let closure_ran = std::cell::Cell::new(false);
        let result = limited.escalate(|_full| {
            closure_ran.set(true);
            1u32
        });
        assert!(
            matches!(result, Err(Error::GpuError(_))),
            "absent FullAccess vtable must return a typed GpuError, got {result:?}"
        );
        assert!(
            !closure_ran.get(),
            "closure must not run when no FullAccess vtable is installed"
        );
    }

    /// Happy-path compile + round-trip lock, gated on the `hardware-tests`
    /// feature (needs a live GPU host; see `docs/testing-hardware.md`). Locks
    /// that `limited.escalate(|full| full.create_texture_readback(...))`
    /// type-checks and the acquired readback is usable on its own scope-free
    /// methods. There is no way to mint a host-backed `GpuContextLimitedAccess`
    /// from the SDK's own test binary, so this cannot run here — /verify-live
    /// exercises it end-to-end through a host-loaded consumer. The null-handle
    /// instance keeps an accidental run under the feature to a clean `Err`
    /// rather than UB.
    #[test]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "needs a live GPU host; /verify-live runs it"
    )]
    fn escalate_create_texture_readback_round_trip() {
        fn round_trip(limited: &GpuContextLimitedAccess) -> Result<u64> {
            let readback = limited.escalate(|full| {
                full.create_texture_readback(
                    "escalate-round-trip",
                    64,
                    64,
                    TextureFormat::Rgba8Unorm,
                )
            })??;
            // Per-frame use rides the readback's OWN methods — scope-free, no
            // second escalate.
            Ok(readback.staging_size())
        }
        let limited = GpuContextLimitedAccess {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
        };
        let _ = round_trip(&limited);
    }

    fn null_full_access() -> GpuContextFullAccess {
        GpuContextFullAccess {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
            handle_kind: HandleKind::ScopeToken,
            inherited_lim_handle: std::ptr::null(),
            inherited_lim_vtable: std::ptr::null(),
        }
    }

    #[test]
    fn gpu_capabilities_on_null_vtable_returns_typed_error_not_ub() {
        // Mental-revert the `self.vtable.is_null()` guard in
        // `gpu_capabilities` and this UB-derefs the null vtable to reach its
        // `gpu_capabilities` slot.
        let full = null_full_access();
        let result = full.gpu_capabilities();
        assert!(
            matches!(result, Err(Error::GpuError(_))),
            "null-vtable gpu_capabilities must return a typed GpuError, got {result:?}"
        );
    }

    #[test]
    fn import_dma_buf_storage_buffer_on_null_vtable_returns_typed_error_not_ub() {
        // Mental-revert the `self.vtable.is_null()` guard in
        // `import_dma_buf_storage_buffer` and this UB-derefs the null vtable.
        // The caller retains the fd on this guard path (the host never sees
        // it), so passing `-1` is safe.
        let full = null_full_access();
        let result = full.import_dma_buf_storage_buffer(-1, 4096);
        assert!(
            matches!(result, Err(Error::GpuError(_))),
            "null-vtable import_dma_buf_storage_buffer must return a typed GpuError, got {result:?}"
        );
    }

    #[test]
    fn copy_texture_to_storage_buffer_and_signal_on_null_vtable_returns_typed_error_not_ub() {
        // The GAP-B-fixed method still guards the null vtable before touching
        // the copy slot or dereferencing any timeline handle. Mental-revert
        // the `self.vtable.is_null()` guard and this UB-derefs the null vtable.
        let full = null_full_access();
        let texture = crate::rhi::Texture {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
            width_cached: 0,
            height_cached: 0,
            format_raw: 0,
            _padding: 0,
        };
        let storage_buffer = crate::rhi::StorageBuffer {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
            byte_size_cached: 0,
            mapped_ptr_cached: std::ptr::null_mut(),
        };
        let result = full.copy_texture_to_storage_buffer_and_signal(
            &texture,
            VulkanLayout(0),
            &storage_buffer,
            None,
            None,
        );
        assert!(
            matches!(result, Err(Error::GpuError(_))),
            "null-vtable copy_texture_to_storage_buffer_and_signal must return a typed GpuError, \
             got {result:?}"
        );
    }

    #[test]
    fn gpu_capabilities_from_repr_reads_fields() {
        // Construct a repr with a known device name + capability bools and
        // assert the u8→bool and UTF-8-slice→String projection.
        let mut device_name = [0u8; 256];
        let name = b"Test GPU 9000";
        device_name[..name.len()].copy_from_slice(name);
        let repr = GpuCapabilitiesRepr {
            device_name,
            device_name_len: name.len() as u32,
            supports_external_memory: 1,
            supports_cross_device_dma_buf_probe: 0,
            supports_ray_tracing_pipeline: 1,
            _reserved_padding: 0,
        };
        let caps = gpu_capabilities_from_repr(&repr);
        assert_eq!(caps.device_name, "Test GPU 9000");
        assert!(caps.supports_external_memory);
        assert!(!caps.supports_cross_device_dma_buf_probe);
        assert!(caps.supports_ray_tracing_pipeline);
    }
}
