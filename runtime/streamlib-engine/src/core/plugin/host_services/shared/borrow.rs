// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side helpers that reconstruct borrowed PluginAbiObjects from the raw
//! handle pointers that cross the vtable.
//!
//! Each `make_*_borrow` populates the cached POD fields on the
//! reconstructed PluginAbiObject from the host-side inner we hold via
//! `handle`. Cdylib PluginAbiObjects carry these cached for free-on-deref
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
    host_vulkan_acceleration_structure_methods_vtable, host_vulkan_compute_kernel_methods_vtable,
    host_vulkan_graphics_kernel_methods_vtable,
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
    // we already have via `handle`. Cdylib PluginAbiObjects carry these cached
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
    // Inner. With #955 the PluginAbiObject's `kind()` / `device_address()` /
    // `storage_size()` getters read the cached fields (no host_inner()
    // fallback), so the borrow MUST carry real values — the
    // ray-tracing kernel's `set_acceleration_structure` check reads
    // `tlas.kind()` and would see BottomLevel for every borrow if the
    // cached field stayed 0.
    let (cached_kind, cached_device_address, cached_storage_size) = if handle.is_null() {
        (0u32, 0u64, 0u64)
    } else {
        // SAFETY: caller hands us a `handle` minted by
        // `Arc::into_raw(Arc<VulkanAccelerationStructureInner>)`,
        // so dereferencing through the host-internal Inner is
        // sound on the host side (this helper is host-only;
        // cdylib borrows would never reach this code path).
        let as_inner =
            unsafe { &*(handle as *const crate::vulkan::rhi::VulkanAccelerationStructureInner) };
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

/// Reconstruct a borrowed `VulkanComputeKernel` PluginAbiObject from its
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

/// Reconstruct a borrowed `VulkanGraphicsKernel` PluginAbiObject from its
/// `Arc::into_raw`-shaped handle. Same pattern as
/// `make_compute_kernel_borrow` — cached POD fields are populated by
/// reading the host-side inner.
pub(in crate::core::plugin::host_services) fn make_graphics_kernel_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::vulkan::rhi::VulkanGraphicsKernel> {
    let k_for_inner = std::mem::ManuallyDrop::new(crate::vulkan::rhi::VulkanGraphicsKernel {
        handle,
        vtable: host_gpu_context_full_access_vtable(),
        methods_vtable: host_vulkan_graphics_kernel_methods_vtable(),
        cached_push_constant_size: 0,
        cached_descriptor_sets_in_flight: 0,
    });
    let inner = k_for_inner.host_inner();
    std::mem::ManuallyDrop::new(crate::vulkan::rhi::VulkanGraphicsKernel {
        handle,
        vtable: host_gpu_context_full_access_vtable(),
        methods_vtable: host_vulkan_graphics_kernel_methods_vtable(),
        cached_push_constant_size: inner.push_constant_size(),
        cached_descriptor_sets_in_flight: inner.descriptor_sets_in_flight(),
    })
}
#[cfg(all(test, target_os = "linux"))]
mod make_borrow_cached_field_regression_tests {
    //! Locks the issue #988 bug: `make_*_borrow` helpers MUST populate
    //! the PluginAbiObject's cached POD fields from the host-side inner —
    //! NOT leave them zeroed. Reverting any `make_*_borrow` to
    //! `width_cached: 0` / `byte_size_cached: 0` / etc. trips these
    //! assertions.
    //!
    //! Requires a working Vulkan device; skips cleanly when one isn't
    //! available (per `project_ci_strategy_no_gpu`).
    use super::*;
    use std::sync::Arc;

    fn try_vulkan_device() -> Option<Arc<crate::vulkan::rhi::HostVulkanDevice>> {
        crate::vulkan::rhi::HostVulkanDevice::new().ok()
    }

    #[test]
    fn make_texture_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let desc = crate::core::rhi::TextureDescriptor::new(
            640,
            480,
            crate::core::rhi::TextureFormat::Rgba8Unorm,
        );
        let host_texture =
            crate::vulkan::rhi::HostVulkanTexture::new(&device, &desc).expect("texture allocate");
        use crate::host_rhi::HostTextureExt;
        let texture = crate::core::rhi::Texture::from_vulkan(host_texture);
        let borrow = make_texture_borrow(texture.handle);
        assert_eq!(borrow.width(), 640, "width_cached must mirror the inner");
        assert_eq!(borrow.height(), 480, "height_cached must mirror the inner");
        assert!(
            matches!(borrow.format(), crate::core::rhi::TextureFormat::Rgba8Unorm),
            "format_raw must mirror the inner"
        );
    }

    #[test]
    fn make_storage_buffer_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let host_buffer =
            crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(&device, 16_384)
                .expect("storage buffer allocate");
        let buffer = crate::core::rhi::StorageBuffer::from_arc_into_raw(Arc::new(host_buffer));
        let borrow = make_storage_buffer_borrow(buffer.handle);
        assert_eq!(
            borrow.byte_size(),
            16_384,
            "byte_size_cached must mirror the inner"
        );
        assert!(
            !borrow.mapped_ptr().is_null(),
            "mapped_ptr_cached must mirror the inner HOST_VISIBLE pointer"
        );
    }

    #[test]
    fn make_pixel_buffer_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        // Bgra8 = 4 bytes/pixel, 320x240 = 307_200 bytes
        let host_buffer = crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(
            &device,
            320 * 240 * 4,
        )
        .expect("backing buffer allocate");
        let pb = crate::core::rhi::PixelBuffer::from_host_vulkan_buffer(
            Arc::new(host_buffer),
            320,
            240,
            4,
            crate::core::rhi::PixelFormat::Bgra32,
        );
        let borrow = make_pixel_buffer_borrow(pb.handle);
        assert_eq!(borrow.width, 320, "width must mirror the inner");
        assert_eq!(borrow.height, 240, "height must mirror the inner");
        assert!(
            matches!(borrow.format(), crate::core::rhi::PixelFormat::Bgra32),
            "format_raw must mirror the inner"
        );
    }

    #[test]
    fn make_uniform_buffer_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let buffer = crate::core::rhi::UniformBuffer::new_host_visible(&device, 4_096)
            .expect("uniform buffer allocate");
        let borrow = make_uniform_buffer_borrow(buffer.handle);
        assert_eq!(
            borrow.byte_size(),
            4_096,
            "byte_size_cached must mirror the inner"
        );
        assert!(
            !borrow.mapped_ptr().is_null(),
            "mapped_ptr_cached must mirror the inner HOST_VISIBLE pointer"
        );
    }

    #[test]
    fn make_vertex_buffer_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let buffer = crate::core::rhi::VertexBuffer::new_host_visible(&device, 8_192)
            .expect("vertex buffer allocate");
        let borrow = make_vertex_buffer_borrow(buffer.handle);
        assert_eq!(
            borrow.byte_size(),
            8_192,
            "byte_size_cached must mirror the inner"
        );
        assert!(
            !borrow.mapped_ptr().is_null(),
            "mapped_ptr_cached must mirror the inner HOST_VISIBLE pointer"
        );
    }

    #[test]
    fn make_index_buffer_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let buffer = crate::core::rhi::IndexBuffer::new_host_visible(&device, 2_048)
            .expect("index buffer allocate");
        let borrow = make_index_buffer_borrow(buffer.handle);
        assert_eq!(
            borrow.byte_size(),
            2_048,
            "byte_size_cached must mirror the inner"
        );
        assert!(
            !borrow.mapped_ptr().is_null(),
            "mapped_ptr_cached must mirror the inner HOST_VISIBLE pointer"
        );
    }

    #[test]
    fn make_compute_kernel_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        // Reuse the test_blend_1 shader already wired in build.rs: one
        // storage buffer binding at slot 0, one push-constant block of
        // 4 bytes. The assertion below pins the value the cached field
        // must mirror.
        const TEST_BLEND_1_SPV: &[u8] =
            include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_1.spv"));
        let descriptor = crate::core::rhi::ComputeKernelDescriptor {
            label: "make_compute_kernel_borrow_test",
            spv: TEST_BLEND_1_SPV,
            bindings: &[
                crate::core::rhi::ComputeBindingSpec::storage_buffer(0),
                crate::core::rhi::ComputeBindingSpec::storage_buffer(8),
            ],
            push_constant_size: 4,
        };
        let kernel = crate::vulkan::rhi::VulkanComputeKernel::new(&device, &descriptor)
            .expect("compute kernel construct");
        let borrow = make_compute_kernel_borrow(kernel.handle);
        assert_eq!(
            borrow.push_constant_size(),
            4,
            "cached_push_constant_size must mirror the inner",
        );
    }

    #[test]
    fn make_acceleration_structure_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        if !device.supports_ray_tracing_pipeline() {
            return;
        }
        // Single triangle BLAS, smallest payload that exercises the
        // build path. Mirrors the rt-smoke fixture's vertex layout.
        let vertices: Vec<f32> = vec![0.0, -0.5, 0.0, -0.5, 0.5, 0.0, 0.5, 0.5, 0.0];
        let indices: Vec<u32> = vec![0, 1, 2];
        let blas = crate::vulkan::rhi::VulkanAccelerationStructure::build_triangles_blas(
            &device,
            "make_borrow_test_blas",
            &vertices,
            &indices,
        )
        .expect("blas construct");
        let borrow = make_acceleration_structure_borrow(blas.handle);
        assert!(
            matches!(
                borrow.kind(),
                crate::vulkan::rhi::AccelerationStructureKind::BottomLevel,
            ),
            "cached_kind must mirror the inner",
        );
        assert!(
            borrow.device_address() > 0,
            "cached_device_address must mirror the inner",
        );
        assert!(
            borrow.storage_size() > 0,
            "cached_storage_size must mirror the inner",
        );
    }
}
