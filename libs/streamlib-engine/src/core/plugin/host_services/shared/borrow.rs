// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side helpers that reconstruct borrowed β-shapes from the raw
//! handle pointers that cross the vtable.
//!
//! Each `make_*_borrow` populates the cached POD fields on the
//! reconstructed β-shape from the host-side inner we hold via
//! `handle`. Cdylib β-shapes carry these cached for free-on-deref
//! POD getters (`width()`, `height()`, `mapped_ptr()`, etc.); when
//! the host reconstructs a borrow inside a vtable callback for code
//! that reads those getters host-side, the borrow's cached fields
//! MUST hold the real values — not zero. Reading zero from a "borrow"
//! of an otherwise-valid resource was the bug behind issue #988
//! (camera-as-cdylib color converter received width=0/height=0 in
//! push constants → kernel produced zero-filled output).
//!
//! `ManuallyDrop` is **load-bearing**, not defensive: removing it
//! would let the borrow's Drop run on scope exit, which calls the
//! vtable's `drop_*` slot and decrements the host's Arc refcount —
//! while the cdylib still holds an outstanding plugin handle that
//! expects to own a strong reference. The result is an under-counted
//! Arc and a use-after-free on the cdylib's eventual Drop.

use std::ffi::c_void;

use super::super::{
    host_gpu_context_full_access_vtable, host_gpu_context_limited_access_vtable,
    host_vulkan_acceleration_structure_methods_vtable,
    host_vulkan_compute_kernel_methods_vtable, host_vulkan_graphics_kernel_methods_vtable,
};

pub(in crate::core::plugin::host_services) fn make_pixel_buffer_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::core::rhi::PixelBuffer> {
    use crate::host_rhi::HostPixelBufferRefExt;
    // Reconstruct a minimal Pixel-buffer borrow whose `buffer_ref()`
    // can read the host-side `PixelBufferRef` we already hold via
    // `handle`.
    let pb_for_inner = std::mem::ManuallyDrop::new(crate::core::rhi::PixelBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        width: 0,
        height: 0,
        format_raw: 0,
        plane_count_cached: 0,
    });
    let pb_ref = pb_for_inner.buffer_ref();
    let hvb = pb_ref.vulkan_inner();
    let format = pb_ref.format();
    std::mem::ManuallyDrop::new(crate::core::rhi::PixelBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        width: pb_ref.width(),
        height: pb_ref.height(),
        format_raw: format as u32,
        plane_count_cached: hvb.plane_count() as u32,
    })
}

pub(in crate::core::plugin::host_services) fn make_storage_buffer_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::core::rhi::StorageBuffer> {
    let sb_for_inner = std::mem::ManuallyDrop::new(crate::core::rhi::StorageBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: 0,
        mapped_ptr_cached: std::ptr::null_mut(),
    });
    let hvb = sb_for_inner.host_inner();
    std::mem::ManuallyDrop::new(crate::core::rhi::StorageBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: hvb.size() as u64,
        mapped_ptr_cached: hvb.mapped_ptr(),
    })
}

pub(in crate::core::plugin::host_services) fn make_uniform_buffer_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::core::rhi::UniformBuffer> {
    let ub_for_inner = std::mem::ManuallyDrop::new(crate::core::rhi::UniformBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: 0,
        mapped_ptr_cached: std::ptr::null_mut(),
    });
    let hvb = ub_for_inner.host_inner();
    std::mem::ManuallyDrop::new(crate::core::rhi::UniformBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: hvb.size() as u64,
        mapped_ptr_cached: hvb.mapped_ptr(),
    })
}

pub(in crate::core::plugin::host_services) fn make_texture_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::core::rhi::Texture> {
    // Populate the cached POD fields from the host-side TextureInner
    // we already have via `handle`. Cdylib β-shapes carry these cached
    // for free-on-deref POD getters (`Texture::width()`, etc.); when
    // the host reconstructs a borrow inside a vtable callback for
    // host-side code that reads `Texture::width()` / `height()`, the
    // borrow's cached fields MUST hold the real values — not zero —
    // because that's what those POD getters return. Reading zero from
    // a "borrow" of an otherwise-valid texture caused the camera-as-
    // cdylib color-converter push constants to encode width=0/height=0
    // and the compute kernel produced zero-filled output (issue #988
    // debug).
    use crate::host_rhi::HostTextureExt;
    let tex_for_inner = std::mem::ManuallyDrop::new(crate::core::rhi::Texture {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        width_cached: 0,
        height_cached: 0,
        format_raw: 0,
        _padding: 0,
    });
    let hvt = tex_for_inner.vulkan_inner();
    let width = hvt.width();
    let height = hvt.height();
    let format = hvt.format();
    std::mem::ManuallyDrop::new(crate::core::rhi::Texture {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        width_cached: width,
        height_cached: height,
        format_raw: format as u32,
        _padding: 0,
    })
}

pub(in crate::core::plugin::host_services) fn make_vertex_buffer_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::core::rhi::VertexBuffer> {
    let vb_for_inner = std::mem::ManuallyDrop::new(crate::core::rhi::VertexBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: 0,
        mapped_ptr_cached: std::ptr::null_mut(),
    });
    let hvb = vb_for_inner.host_inner();
    std::mem::ManuallyDrop::new(crate::core::rhi::VertexBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: hvb.size() as u64,
        mapped_ptr_cached: hvb.mapped_ptr(),
    })
}

pub(in crate::core::plugin::host_services) fn make_index_buffer_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::core::rhi::IndexBuffer> {
    let ib_for_inner = std::mem::ManuallyDrop::new(crate::core::rhi::IndexBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: 0,
        mapped_ptr_cached: std::ptr::null_mut(),
    });
    let hvb = ib_for_inner.host_inner();
    std::mem::ManuallyDrop::new(crate::core::rhi::IndexBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: hvb.size() as u64,
        mapped_ptr_cached: hvb.mapped_ptr(),
    })
}

pub(in crate::core::plugin::host_services) fn make_acceleration_structure_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::vulkan::rhi::VulkanAccelerationStructure> {
    // Read the cached POD descriptors directly from the host-internal
    // Inner. With #955 the β-shape's `kind()` / `device_address()` /
    // `storage_size()` getters read the cached fields (no host_inner()
    // fallback), so the borrow MUST carry real values — the
    // ray-tracing kernel's `set_acceleration_structure` check reads
    // `tlas.kind()` and would see BottomLevel for every borrow if the
    // cached field stayed 0.
    let (cached_kind, cached_device_address, cached_storage_size) =
        if handle.is_null() {
            (0u32, 0u64, 0u64)
        } else {
            // SAFETY: caller hands us a `handle` minted by
            // `Arc::into_raw(Arc<VulkanAccelerationStructureInner>)`,
            // so dereferencing through the host-internal Inner is
            // sound on the host side (this helper is host-only;
            // cdylib borrows would never reach this code path).
            let as_inner = unsafe {
                &*(handle as *const crate::vulkan::rhi::VulkanAccelerationStructureInner)
            };
            let kind = match as_inner.kind() {
                crate::vulkan::rhi::AccelerationStructureKind::BottomLevel => 0u32,
                crate::vulkan::rhi::AccelerationStructureKind::TopLevel => 1u32,
            };
            (kind, as_inner.device_address(), as_inner.storage_size())
        };
    std::mem::ManuallyDrop::new(crate::vulkan::rhi::VulkanAccelerationStructure {
        handle,
        vtable: host_gpu_context_full_access_vtable(),
        methods_vtable: host_vulkan_acceleration_structure_methods_vtable(),
        cached_kind,
        _reserved_padding: 0,
        cached_device_address,
        cached_storage_size,
    })
}

/// Reconstruct a borrowed `VulkanComputeKernel` β-shape from its
/// `Arc::into_raw`-shaped handle. Cached POD fields are populated by
/// reading the host-side inner.
///
/// The vtable + methods_vtable pointers are filled with the host's
/// own statics (matching what `from_arc_into_raw` would have written
/// in host mode) so the borrow is well-formed for any field-only
/// read even though no vtable callback is supposed to fire while the
/// borrow is alive.
pub(in crate::core::plugin::host_services) fn make_compute_kernel_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::vulkan::rhi::VulkanComputeKernel> {
    let k_for_inner = std::mem::ManuallyDrop::new(crate::vulkan::rhi::VulkanComputeKernel {
        handle,
        vtable: host_gpu_context_full_access_vtable(),
        methods_vtable: host_vulkan_compute_kernel_methods_vtable(),
        cached_push_constant_size: 0,
        _reserved_padding: 0,
    });
    let inner = k_for_inner.host_inner();
    std::mem::ManuallyDrop::new(crate::vulkan::rhi::VulkanComputeKernel {
        handle,
        vtable: host_gpu_context_full_access_vtable(),
        methods_vtable: host_vulkan_compute_kernel_methods_vtable(),
        cached_push_constant_size: inner.push_constant_size(),
        _reserved_padding: 0,
    })
}

/// Reconstruct a borrowed `VulkanGraphicsKernel` β-shape from its
/// `Arc::into_raw`-shaped handle. Same pattern as
/// `make_compute_kernel_borrow` — cached POD fields are populated by
/// reading the host-side inner.
pub(in crate::core::plugin::host_services) fn make_graphics_kernel_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::vulkan::rhi::VulkanGraphicsKernel> {
    let k_for_inner = std::mem::ManuallyDrop::new(
        crate::vulkan::rhi::VulkanGraphicsKernel {
            handle,
            vtable: host_gpu_context_full_access_vtable(),
            methods_vtable: host_vulkan_graphics_kernel_methods_vtable(),
            cached_push_constant_size: 0,
            cached_descriptor_sets_in_flight: 0,
        },
    );
    let inner = k_for_inner.host_inner();
    std::mem::ManuallyDrop::new(crate::vulkan::rhi::VulkanGraphicsKernel {
        handle,
        vtable: host_gpu_context_full_access_vtable(),
        methods_vtable: host_vulkan_graphics_kernel_methods_vtable(),
        cached_push_constant_size: inner.push_constant_size(),
        cached_descriptor_sets_in_flight: inner.descriptor_sets_in_flight(),
    })
}
