// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's [`VulkanGraphicsKernel`] PluginAbiObject.
//!
//! Layout-stable `#[repr(C)] { handle, vtable, methods_vtable, cached
//! POD }` so cdylibs can hold, refcount, drop, and read POD descriptors
//! without sharing rustc-version or dep-graph with the host. The opaque
//! handle points at the host's `Arc<VulkanGraphicsKernelInner>`; lifecycle
//! (Clone / Drop) dispatches through the parent
//! [`GpuContextFullAccessVTable`]'s `clone_graphics_kernel` /
//! `drop_graphics_kernel` callbacks, and per-method dispatch is reached
//! through the per-type
//! [`streamlib_plugin_abi::VulkanGraphicsKernelMethodsVTable`].
//!
//! Engine-free coverage is the fullscreen-fragment-effect path: sampled-
//! texture / storage-image / storage-buffer binding setters, push
//! constants, and the owned-command-buffer `offscreen_render`. The
//! vertex/index/uniform-buffer setters and the raw-`vk::CommandBuffer`
//! `cmd_bind_and_draw*` slots stay engine-side — they name buffer twins
//! the engine-free SDK doesn't carry, or `vulkanalia` types that can't
//! cross the boundary.

use std::ffi::c_void;

use streamlib_error::{Error, Result};
use streamlib_plugin_abi::{GpuContextFullAccessVTable, VulkanGraphicsKernelMethodsVTable};

use crate::rhi::{OffscreenColorTarget, OffscreenDraw, PixelBuffer, StorageBuffer, Texture};

/// Graphics kernel — layout-stable `#[repr(C)]` PluginAbiObject.
///
/// The `push_constant_size()` / `descriptor_sets_in_flight()` POD getters
/// read cached fields with no plugin ABI hop. Binding setters,
/// push-constants, and `offscreen_render` route through the per-type
/// `methods_vtable`.
#[repr(C)]
pub struct VulkanGraphicsKernel {
    /// Opaque handle to the host's `Arc<VulkanGraphicsKernelInner>`.
    pub(crate) handle: *const c_void,
    /// Parent vtable for plugin ABI Clone/Drop dispatch.
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Per-type vtable for plugin ABI method dispatch.
    pub(crate) methods_vtable: *const VulkanGraphicsKernelMethodsVTable,
    /// Cached push-constant size in bytes. Set at construction; fixed for
    /// the kernel's lifetime.
    pub(crate) cached_push_constant_size: u32,
    /// Cached descriptor-set ring depth. Set at construction; fixed for
    /// the kernel's lifetime.
    pub(crate) cached_descriptor_sets_in_flight: u32,
}

// SAFETY: handle points at an `Arc<VulkanGraphicsKernelInner>` whose
// interior is Send+Sync (the dispatch fence serializes GPU work; the
// per-frame descriptor-set ring serializes setter writes). Refcount +
// method dispatch run in host-compiled code through the vtables.
unsafe impl Send for VulkanGraphicsKernel {}
unsafe impl Sync for VulkanGraphicsKernel {}

impl VulkanGraphicsKernel {
    /// Bind a raw-bytes [`StorageBuffer`] at `(frame_index, binding)`.
    /// Dispatches through the per-type methods vtable's
    /// `set_storage_buffer_storage` slot.
    pub fn set_storage_buffer_storage(
        &self,
        frame_index: u32,
        binding: u32,
        buffer: &StorageBuffer,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_storage_buffer_storage: graphics kernel methods vtable is null".into(),
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
                frame_index,
                binding,
                buffer.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// Bind a [`PixelBuffer`]-shaped storage buffer (SSBO) at
    /// `(frame_index, binding)`. Dispatches through the per-type methods
    /// vtable's `set_storage_buffer_pixel` slot.
    pub fn set_storage_buffer_pixel(
        &self,
        frame_index: u32,
        binding: u32,
        buffer: &PixelBuffer,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_storage_buffer_pixel: graphics kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: see set_storage_buffer_storage.
        let status = unsafe {
            ((*self.methods_vtable).set_storage_buffer_pixel)(
                self.handle,
                frame_index,
                binding,
                buffer.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// Bind a sampled texture at `(frame_index, binding)`, using the
    /// kernel's default linear-clamp sampler. Dispatches through the
    /// per-type methods vtable's `set_sampled_texture` slot.
    pub fn set_sampled_texture(
        &self,
        frame_index: u32,
        binding: u32,
        texture: &Texture,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_sampled_texture: graphics kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: see set_storage_buffer_storage.
        let status = unsafe {
            ((*self.methods_vtable).set_sampled_texture)(
                self.handle,
                frame_index,
                binding,
                texture.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// Bind a storage image at `(frame_index, binding)`. Caller guarantees
    /// the texture's `STORAGE_BINDING` usage was declared at creation.
    /// Dispatches through the per-type methods vtable's `set_storage_image`
    /// slot.
    pub fn set_storage_image(
        &self,
        frame_index: u32,
        binding: u32,
        texture: &Texture,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_storage_image: graphics kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: see set_storage_buffer_storage.
        let status = unsafe {
            ((*self.methods_vtable).set_storage_image)(
                self.handle,
                frame_index,
                binding,
                texture.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// Stage push-constant bytes for `frame_index`. Dispatches through the
    /// per-type methods vtable's `set_push_constants` slot.
    pub fn set_push_constants(&self, frame_index: u32, bytes: &[u8]) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_push_constants: graphics kernel methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: see set_storage_buffer_storage; `bytes` is read-only and
        // consumed inside the call.
        let status = unsafe {
            ((*self.methods_vtable).set_push_constants)(
                self.handle,
                frame_index,
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
    pub fn set_push_constants_value<T: Copy>(&self, frame_index: u32, value: &T) -> Result<()> {
        // SAFETY: T is Copy + Sized so its layout is stable; the byte view
        // is read-only and consumed inside the plugin ABI call.
        let bytes = unsafe {
            std::slice::from_raw_parts(value as *const T as *const u8, std::mem::size_of::<T>())
        };
        self.set_push_constants(frame_index, bytes)
    }

    /// Render into one or more offscreen color attachments using the
    /// kernel's owned command buffer + fence. Dispatches through the
    /// per-type methods vtable's `offscreen_render` slot.
    ///
    /// `color_targets` marshal into the plugin ABI's parallel-array shape
    /// (texture handles / clear-present flags / clear values). `extent` is
    /// the `(width, height)` render area. `draw` is the [`OffscreenDraw`]
    /// tagged union.
    pub fn offscreen_render(
        &self,
        frame_index: u32,
        color_targets: &[OffscreenColorTarget<'_>],
        extent: (u32, u32),
        draw: OffscreenDraw,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "offscreen_render: graphics kernel methods vtable is null".into(),
            ));
        }
        // Marshal color targets into the plugin ABI's parallel-array shape.
        let mut handles: Vec<*const c_void> = Vec::with_capacity(color_targets.len());
        let mut present_flags: Vec<u32> = Vec::with_capacity(color_targets.len());
        let mut clear_values: Vec<[f32; 4]> = Vec::with_capacity(color_targets.len());
        for target in color_targets {
            handles.push(target.texture.handle);
            if let Some(c) = target.clear_color {
                present_flags.push(1);
                clear_values.push(c);
            } else {
                present_flags.push(0);
                clear_values.push([0.0, 0.0, 0.0, 0.0]);
            }
        }

        let draw_repr = encode_offscreen_draw_repr(&draw);

        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard above; handle was
        // paired with it at mint time. The parallel arrays each have
        // length `handles.len()` and outlive the call; `draw_repr` lives
        // on this stack frame across the call.
        let status = unsafe {
            ((*self.methods_vtable).offscreen_render)(
                self.handle,
                frame_index,
                if handles.is_empty() {
                    std::ptr::null()
                } else {
                    handles.as_ptr()
                },
                if present_flags.is_empty() {
                    std::ptr::null()
                } else {
                    present_flags.as_ptr()
                },
                if clear_values.is_empty() {
                    std::ptr::null()
                } else {
                    clear_values.as_ptr()
                },
                handles.len(),
                extent.0,
                extent.1,
                &draw_repr,
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

    /// Descriptor-set ring depth. Cached POD — no plugin ABI hop. Render-
    /// loop callers pass `frame_index ∈ [0, descriptor_sets_in_flight())`
    /// to the binding setters + `offscreen_render`.
    pub fn descriptor_sets_in_flight(&self) -> u32 {
        self.cached_descriptor_sets_in_flight
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

fn viewport_to_repr(v: super::Viewport) -> streamlib_plugin_abi::ViewportRepr {
    streamlib_plugin_abi::ViewportRepr {
        x: v.x,
        y: v.y,
        width: v.width,
        height: v.height,
        min_depth: v.min_depth,
        max_depth: v.max_depth,
    }
}

fn scissor_to_repr(s: super::ScissorRect) -> streamlib_plugin_abi::ScissorRectRepr {
    streamlib_plugin_abi::ScissorRectRepr {
        x: s.x,
        y: s.y,
        width: s.width,
        height: s.height,
    }
}

fn zero_viewport_repr() -> streamlib_plugin_abi::ViewportRepr {
    streamlib_plugin_abi::ViewportRepr {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
        min_depth: 0.0,
        max_depth: 0.0,
    }
}

fn zero_scissor_repr() -> streamlib_plugin_abi::ScissorRectRepr {
    streamlib_plugin_abi::ScissorRectRepr {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    }
}

fn draw_call_to_repr(d: &super::DrawCall) -> streamlib_plugin_abi::DrawCallRepr {
    let (viewport_present, viewport) = match d.viewport {
        Some(v) => (1u32, viewport_to_repr(v)),
        None => (0u32, zero_viewport_repr()),
    };
    let (scissor_present, scissor) = match d.scissor {
        Some(s) => (1u32, scissor_to_repr(s)),
        None => (0u32, zero_scissor_repr()),
    };
    streamlib_plugin_abi::DrawCallRepr {
        vertex_count: d.vertex_count,
        instance_count: d.instance_count,
        first_vertex: d.first_vertex,
        first_instance: d.first_instance,
        viewport_present,
        scissor_present,
        viewport,
        scissor,
    }
}

fn draw_indexed_call_to_repr(d: &super::DrawIndexedCall) -> streamlib_plugin_abi::DrawIndexedCallRepr {
    let (viewport_present, viewport) = match d.viewport {
        Some(v) => (1u32, viewport_to_repr(v)),
        None => (0u32, zero_viewport_repr()),
    };
    let (scissor_present, scissor) = match d.scissor {
        Some(s) => (1u32, scissor_to_repr(s)),
        None => (0u32, zero_scissor_repr()),
    };
    streamlib_plugin_abi::DrawIndexedCallRepr {
        index_count: d.index_count,
        instance_count: d.instance_count,
        first_index: d.first_index,
        vertex_offset: d.vertex_offset,
        first_instance: d.first_instance,
        viewport_present,
        scissor_present,
        _reserved_padding: 0,
        viewport,
        scissor,
    }
}

fn encode_offscreen_draw_repr(draw: &OffscreenDraw) -> streamlib_plugin_abi::OffscreenDrawRepr {
    let zero_draw = streamlib_plugin_abi::DrawCallRepr {
        vertex_count: 0,
        instance_count: 0,
        first_vertex: 0,
        first_instance: 0,
        viewport_present: 0,
        scissor_present: 0,
        viewport: zero_viewport_repr(),
        scissor: zero_scissor_repr(),
    };
    let zero_draw_indexed = streamlib_plugin_abi::DrawIndexedCallRepr {
        index_count: 0,
        instance_count: 0,
        first_index: 0,
        vertex_offset: 0,
        first_instance: 0,
        viewport_present: 0,
        scissor_present: 0,
        _reserved_padding: 0,
        viewport: zero_viewport_repr(),
        scissor: zero_scissor_repr(),
    };
    match draw {
        OffscreenDraw::Draw(d) => streamlib_plugin_abi::OffscreenDrawRepr {
            kind: streamlib_plugin_abi::OffscreenDrawKindRepr::Draw as u32,
            _reserved_padding: 0,
            draw_call: draw_call_to_repr(d),
            draw_indexed_call: zero_draw_indexed,
        },
        OffscreenDraw::DrawIndexed(d) => streamlib_plugin_abi::OffscreenDrawRepr {
            kind: streamlib_plugin_abi::OffscreenDrawKindRepr::DrawIndexed as u32,
            _reserved_padding: 0,
            draw_call: zero_draw,
            draw_indexed_call: draw_indexed_call_to_repr(d),
        },
    }
}

impl Clone for VulkanGraphicsKernel {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle paired at mint time; the vtable's
            // `clone_graphics_kernel` contract is
            // `Arc::increment_strong_count` host-side.
            unsafe {
                ((*self.vtable).clone_graphics_kernel)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            vtable: self.vtable,
            methods_vtable: self.methods_vtable,
            cached_push_constant_size: self.cached_push_constant_size,
            cached_descriptor_sets_in_flight: self.cached_descriptor_sets_in_flight,
        }
    }
}

impl Drop for VulkanGraphicsKernel {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the host's `Arc::into_raw` and any
            // `clone_graphics_kernel` bumps.
            unsafe {
                ((*self.vtable).drop_graphics_kernel)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for VulkanGraphicsKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanGraphicsKernel").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn vulkan_graphics_kernel_layout() {
        // Must match the engine's
        // `vulkan/rhi/vulkan_graphics_kernel.rs::VulkanGraphicsKernel`:
        //   handle @ 0, vtable @ 8, methods_vtable @ 16,
        //   cached_push_constant_size @ 24,
        //   cached_descriptor_sets_in_flight @ 28.
        // Total 32 bytes, align 8.
        assert_eq!(size_of::<VulkanGraphicsKernel>(), 32);
        assert_eq!(align_of::<VulkanGraphicsKernel>(), 8);
        assert_eq!(offset_of!(VulkanGraphicsKernel, handle), 0);
        assert_eq!(offset_of!(VulkanGraphicsKernel, vtable), 8);
        assert_eq!(offset_of!(VulkanGraphicsKernel, methods_vtable), 16);
        assert_eq!(
            offset_of!(VulkanGraphicsKernel, cached_push_constant_size),
            24
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernel, cached_descriptor_sets_in_flight),
            28
        );
    }

    #[test]
    fn vulkan_graphics_kernel_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<VulkanGraphicsKernel>();
    }
}
