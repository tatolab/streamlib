// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's [`VulkanComputeKernel`] PluginAbiObject.
//!
//! Layout-stable `#[repr(C)] { handle, vtable, methods_vtable, cached
//! POD }` so cdylibs can hold, refcount, drop, and read POD descriptors
//! without sharing rustc-version or dep-graph with the host. The opaque
//! handle points at the host's `Arc<VulkanComputeKernelInner>`; lifecycle
//! (Clone / Drop) dispatches through the parent
//! [`GpuContextFullAccessVTable`]'s `clone_compute_kernel` /
//! `drop_compute_kernel` callbacks, and per-method dispatch is reached
//! through the per-type
//! [`streamlib_plugin_abi::VulkanComputeKernelMethodsVTable`].
//!
//! The host `VulkanComputeKernelInner` backing + the raw-`HostVulkanDevice`
//! constructors + the `vk::ImageView` / `record` raw-vulkanalia escape
//! hatches stay in the engine (they name `vulkanalia` types that can't
//! cross the engine-free boundary).

use std::ffi::c_void;

use streamlib_error::{Error, Result};
use streamlib_plugin_abi::{GpuContextFullAccessVTable, VulkanComputeKernelMethodsVTable};

use crate::rhi::{StorageBuffer, Texture};

/// Compute kernel — layout-stable `#[repr(C)]` PluginAbiObject.
///
/// The `push_constant_size()` POD getter reads the cached field with no
/// plugin ABI hop. Binding setters + `dispatch` route through the
/// per-type `methods_vtable`.
#[repr(C)]
pub struct VulkanComputeKernel {
    /// Opaque handle to the host's `Arc<VulkanComputeKernelInner>`.
    pub(crate) handle: *const c_void,
    /// Parent vtable for plugin ABI Clone/Drop dispatch.
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Per-type vtable for plugin ABI method dispatch.
    pub(crate) methods_vtable: *const VulkanComputeKernelMethodsVTable,
    /// Cached push-constant size in bytes. Set at construction; fixed
    /// for the kernel's lifetime.
    pub(crate) cached_push_constant_size: u32,
    /// Reserved padding so the struct stays 8-byte aligned.
    pub(crate) _reserved_padding: u32,
}

// SAFETY: handle points at an `Arc<VulkanComputeKernelInner>` whose
// interior is Send+Sync (the dispatch fence serializes GPU work; the
// pending-state mutex serializes setter writes). Refcount + method
// dispatch run in host-compiled code through the vtables.
unsafe impl Send for VulkanComputeKernel {}
unsafe impl Sync for VulkanComputeKernel {}

impl VulkanComputeKernel {
    /// Bind a raw-bytes [`StorageBuffer`] at `binding` — the canonical
    /// shape from
    /// [`crate::context::GpuContextFullAccess::acquire_storage_buffer`].
    /// Dispatches through the per-type methods vtable's
    /// `set_storage_buffer_storage` slot.
    pub fn set_storage_buffer_storage(
        &self,
        binding: u32,
        buffer: &StorageBuffer,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_storage_buffer_storage: kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard above; handle was
        // paired with it at mint time. The buffer handle is the borrowed
        // `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer the host
        // reconstructs.
        let status = unsafe {
            ((*self.methods_vtable).set_storage_buffer_storage)(
                self.handle,
                binding,
                buffer.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// Bind a sampled texture at `binding`, using the kernel's default
    /// linear-clamp sampler. Dispatches through the per-type methods
    /// vtable's `set_sampled_texture` slot.
    pub fn set_sampled_texture(&self, binding: u32, texture: &Texture) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_sampled_texture: kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: see set_storage_buffer_storage.
        let status = unsafe {
            ((*self.methods_vtable).set_sampled_texture)(
                self.handle,
                binding,
                texture.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// Bind a storage image at `binding`. Caller guarantees the texture's
    /// `STORAGE_BINDING` usage was declared at creation. Dispatches
    /// through the per-type methods vtable's `set_storage_image` slot.
    pub fn set_storage_image(&self, binding: u32, texture: &Texture) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_storage_image: kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: see set_storage_buffer_storage.
        let status = unsafe {
            ((*self.methods_vtable).set_storage_image)(
                self.handle,
                binding,
                texture.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// Upload push-constant bytes. Dispatches through the per-type
    /// methods vtable's `set_push_constants` slot.
    pub fn set_push_constants(&self, bytes: &[u8]) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_push_constants: kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: see set_storage_buffer_storage; `bytes` is read-only
        // and consumed inside the call.
        let status = unsafe {
            ((*self.methods_vtable).set_push_constants)(
                self.handle,
                bytes.as_ptr(),
                bytes.len(),
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// Convenience: re-interprets `&T` as a byte slice and forwards to
    /// [`Self::set_push_constants`].
    pub fn set_push_constants_value<T: Copy>(&self, value: &T) -> Result<()> {
        // SAFETY: T is Copy + Sized so its layout is stable; the byte
        // view is read-only and consumed inside the plugin ABI call.
        let bytes = unsafe {
            std::slice::from_raw_parts(value as *const T as *const u8, std::mem::size_of::<T>())
        };
        self.set_push_constants(bytes)
    }

    /// Dispatch the kernel with the given workgroup counts. Dispatches
    /// through the per-type methods vtable's `dispatch` slot.
    pub fn dispatch(&self, group_x: u32, group_y: u32, group_z: u32) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "dispatch: kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: see set_storage_buffer_storage.
        let status = unsafe {
            ((*self.methods_vtable).dispatch)(
                self.handle,
                group_x,
                group_y,
                group_z,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// Push-constant range size in bytes. Cached POD — no plugin ABI hop.
    pub fn push_constant_size(&self) -> u32 {
        self.cached_push_constant_size
    }
}

/// Decode a `(status, err_buf)` plugin-ABI return into `Result<()>`.
fn status_to_result(status: i32, err_buf: &[u8], err_len: usize) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
        Err(Error::GpuError(msg))
    }
}

impl Clone for VulkanComputeKernel {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle paired at mint time; the vtable's
            // `clone_compute_kernel` contract is
            // `Arc::increment_strong_count` host-side.
            unsafe {
                ((*self.vtable).clone_compute_kernel)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            vtable: self.vtable,
            methods_vtable: self.methods_vtable,
            cached_push_constant_size: self.cached_push_constant_size,
            _reserved_padding: self._reserved_padding,
        }
    }
}

impl Drop for VulkanComputeKernel {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the host's `Arc::into_raw` and any
            // `clone_compute_kernel` bumps.
            unsafe {
                ((*self.vtable).drop_compute_kernel)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for VulkanComputeKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanComputeKernel").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn vulkan_compute_kernel_layout() {
        // Must match the engine's
        // `vulkan/rhi/vulkan_compute_kernel.rs::VulkanComputeKernel`:
        //   handle @ 0, vtable @ 8, methods_vtable @ 16,
        //   cached_push_constant_size @ 24, _reserved_padding @ 28.
        // Total 32 bytes, align 8.
        assert_eq!(size_of::<VulkanComputeKernel>(), 32);
        assert_eq!(align_of::<VulkanComputeKernel>(), 8);
        assert_eq!(offset_of!(VulkanComputeKernel, handle), 0);
        assert_eq!(offset_of!(VulkanComputeKernel, vtable), 8);
        assert_eq!(offset_of!(VulkanComputeKernel, methods_vtable), 16);
        assert_eq!(
            offset_of!(VulkanComputeKernel, cached_push_constant_size),
            24
        );
        assert_eq!(offset_of!(VulkanComputeKernel, _reserved_padding), 28);
    }

    #[test]
    fn vulkan_compute_kernel_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<VulkanComputeKernel>();
    }
}
