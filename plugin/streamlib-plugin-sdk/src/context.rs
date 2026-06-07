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
//! Only the GPU-accessor field reads are provided. The ABI-mediated
//! accessors (`runtime_id`, `processor_id`, `audio_clock`, `runtime`,
//! …) are a later phase — the proof CPU-only plugin needs only the two
//! `gpu_*_access()` field reads on the RuntimeContext views.

use std::ffi::c_void;
use std::marker::PhantomData;

use streamlib_plugin_abi::{
    GpuContextFullAccessVTable, GpuContextLimitedAccessVTable, RuntimeContextVTable,
};

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
    pub fn acquire_storage_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::rhi::StorageBuffer> {
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
                "resolve_texture_by_surface_id: GpuContextLimitedAccess has null handle/vtable".into(),
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
    pub fn acquire_storage_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::rhi::StorageBuffer> {
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

    /// Build an engine-owned multi-step command-buffer recorder.
    /// Dispatches through the [`GpuContextFullAccessVTable`]'s
    /// `create_command_recorder` slot.
    pub fn create_command_recorder(
        &self,
        label: &str,
    ) -> Result<crate::rhi::RhiCommandRecorder> {
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
        assert_eq!(offset_of!(RuntimeContextFullAccess<'static>, gpu_limited), 56);
    }

    #[test]
    fn runtime_context_limited_access_layout() {
        assert_eq!(size_of::<RuntimeContextLimitedAccess<'static>>(), 32);
        assert_eq!(align_of::<RuntimeContextLimitedAccess<'static>>(), 8);
        assert_eq!(offset_of!(RuntimeContextLimitedAccess<'static>, handle), 0);
        assert_eq!(offset_of!(RuntimeContextLimitedAccess<'static>, vtable), 8);
        assert_eq!(offset_of!(RuntimeContextLimitedAccess<'static>, gpu_limited), 16);
    }
}
