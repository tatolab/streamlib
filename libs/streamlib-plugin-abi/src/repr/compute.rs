// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Compute-kernel `#[repr(C)]` descriptor mirrors + GPU capability snapshot.

/// `#[repr(u32)]` mirror of `streamlib::core::rhi::ComputeBindingKind`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputeBindingKindRepr {
    StorageBuffer = 0,
    UniformBuffer = 1,
    SampledTexture = 2,
    StorageImage = 3,
    /// `VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE` — sampled image without a
    /// combined sampler. GLSL `texture2D` / `texelFetch` style.
    SampledImage = 4,
}

/// GPU capability snapshot returned by
/// [`GpuContextFullAccessVTable::gpu_capabilities`]. Layout-stable
/// `#[repr(C)]` so cdylibs can read the fields cross-rustc-version
/// without dep-graph coupling.
///
/// `device_name` is a fixed-size UTF-8 buffer; bytes past `device_name_len`
/// are unspecified. The 256-byte buffer matches Vulkan's
/// `VK_MAX_PHYSICAL_DEVICE_NAME_SIZE` (the source string for vendor
/// names). `_reserved_padding` brings the struct to 8-byte alignment.
#[repr(C)]
pub struct GpuCapabilitiesRepr {
    /// UTF-8 device name; valid for `device_name_len` bytes. Trailing
    /// bytes are unspecified.
    pub device_name: [u8; 256],
    /// Number of valid UTF-8 bytes in `device_name`.
    pub device_name_len: u32,
    /// Whether the GPU exposes `VK_KHR_external_memory_fd` +
    /// `VK_EXT_external_memory_dma_buf` (DMA-BUF FD import path
    /// available).
    pub supports_external_memory: u8,
    /// Whether cross-device DMA-BUF probe is supported. NVIDIA Linux
    /// reports `false` per the engine-layer capability guard
    /// (`docs/learnings/nvidia-opaque-fd-after-swapchain.md`).
    pub supports_cross_device_dma_buf_probe: u8,
    /// Whether the GPU exposes `VK_KHR_ray_tracing_pipeline`.
    pub supports_ray_tracing_pipeline: u8,
    /// Reserved — zero today, brings struct to 264-byte natural
    /// alignment with room for future capability bools.
    pub _reserved_padding: u8,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::ComputeBindingSpec`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ComputeBindingSpecRepr {
    pub binding: u32,
    /// `ComputeBindingKindRepr` discriminant. Held as `u32` to keep the
    /// in-FFI value layout-stable across rustc versions (matches the
    /// pattern used by `acquire_texture`'s `format_raw` parameter).
    pub kind: u32,
}

/// `#[repr(C)]` mirror of `streamlib::core::rhi::ComputeKernelDescriptor`.
///
/// All pointer fields borrow into caller-owned memory and must
/// remain valid for the duration of the vtable call.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ComputeKernelDescriptorRepr {
    pub label_ptr: *const u8,
    pub label_len: usize,
    pub spv_ptr: *const u8,
    pub spv_len: usize,
    pub bindings_ptr: *const ComputeBindingSpecRepr,
    pub bindings_len: usize,
    pub push_constant_size: u32,
    pub _reserved_padding: u32,
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn gpu_capabilities_repr_layout() {
        // 256-byte device_name + u32 len + 4 u8 fields = 264 bytes.
        // 1-byte alignment (the byte array has 1-byte alignment, u32 has
        // 4-byte but follows the byte array directly; the trailing bools
        // are u8). Total stable across rustc.
        assert_eq!(size_of::<GpuCapabilitiesRepr>(), 264);
        assert_eq!(align_of::<GpuCapabilitiesRepr>(), 4);
        assert_eq!(offset_of!(GpuCapabilitiesRepr, device_name), 0);
        assert_eq!(offset_of!(GpuCapabilitiesRepr, device_name_len), 256);
        assert_eq!(
            offset_of!(GpuCapabilitiesRepr, supports_external_memory),
            260
        );
        assert_eq!(
            offset_of!(GpuCapabilitiesRepr, supports_cross_device_dma_buf_probe),
            261
        );
        assert_eq!(
            offset_of!(GpuCapabilitiesRepr, supports_ray_tracing_pipeline),
            262
        );
        assert_eq!(offset_of!(GpuCapabilitiesRepr, _reserved_padding), 263);
    }

    // -------------------------------------------------------------------------
    // Phase C2 descriptor mirror layouts
    // -------------------------------------------------------------------------

    #[test]
    fn compute_binding_spec_repr_layout() {
        assert_eq!(size_of::<ComputeBindingSpecRepr>(), 8);
        assert_eq!(align_of::<ComputeBindingSpecRepr>(), 4);
        assert_eq!(offset_of!(ComputeBindingSpecRepr, binding), 0);
        assert_eq!(offset_of!(ComputeBindingSpecRepr, kind), 4);
    }
    #[test]
    fn compute_kernel_descriptor_repr_layout() {
        // 3 (ptr, len) pairs (3 * 16 = 48) + u32 + u32 = 56 bytes on
        // 64-bit hosts.
        assert_eq!(size_of::<ComputeKernelDescriptorRepr>(), 56);
        assert_eq!(align_of::<ComputeKernelDescriptorRepr>(), 8);
        assert_eq!(offset_of!(ComputeKernelDescriptorRepr, label_ptr), 0);
        assert_eq!(offset_of!(ComputeKernelDescriptorRepr, label_len), 8);
        assert_eq!(offset_of!(ComputeKernelDescriptorRepr, spv_ptr), 16);
        assert_eq!(offset_of!(ComputeKernelDescriptorRepr, spv_len), 24);
        assert_eq!(offset_of!(ComputeKernelDescriptorRepr, bindings_ptr), 32);
        assert_eq!(offset_of!(ComputeKernelDescriptorRepr, bindings_len), 40);
        assert_eq!(
            offset_of!(ComputeKernelDescriptorRepr, push_constant_size),
            48
        );
        assert_eq!(
            offset_of!(ComputeKernelDescriptorRepr, _reserved_padding),
            52
        );
    }
}
