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
//! with the host's `AudioClockVTable` cached on `HostServices`. The
//! remaining ABI-mediated accessors (`runtime_id`, `processor_id`,
//! `runtime`, …) are a later phase.

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
}

// =============================================================================
// HandleKind — drop discriminator on GpuContextFullAccess
// =============================================================================

/// Discriminator for [`GpuContextFullAccess`]'s `handle` field. The
/// engine-internal in-process constructor sets `Boxed`; the cdylib
/// vtable-dispatched constructor sets `ScopeToken`. Drop dispatches on
/// this kind.
// The SDK never *constructs* a HandleKind (host-built views arrive by
// pointer); the variants exist for `#[repr(C)]` layout parity and the Drop
// match. Allow them to be "never constructed" within this crate.
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
/// lifecycle call and cannot be stashed.
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
    /// Distinct from the engine-only non-exportable
    /// `create_timeline_semaphore` (Arc-raw transit, in-process): the
    /// returned timeline can `export_opaque_fd` for cross-process /
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
    /// The cross-API timeline wait/signal (`consume_done` / `produce_done`)
    /// rides null handles until #1260 lands the SDK exportable-timeline
    /// PluginAbiObject; on the null path the host submission blocks until
    /// the copy completes. When #1260 lands, this method gains the
    /// `Option<(&HostTimelineSemaphore, u64)>` wait/signal parameters.
    pub fn copy_texture_to_storage_buffer_and_signal(
        &self,
        source_texture: &crate::rhi::Texture,
        source_layout: VulkanLayout,
        dst: &crate::rhi::StorageBuffer,
    ) -> Result<()> {
        if self.vtable.is_null() {
            return Err(Error::GpuError(
                "copy_texture_to_storage_buffer_and_signal: GpuContextFullAccess has null vtable"
                    .into(),
            ));
        }
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vtable + handle paired at construction; the texture +
        // storage buffer are borrowed for the call. Null timeline handles
        // select the host-blocking (no cross-API sync) path.
        let status = unsafe {
            ((*self.vtable).copy_texture_to_storage_buffer_and_signal)(
                self.handle,
                source_texture.handle,
                source_layout.0,
                dst as *const _ as *const c_void,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
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
