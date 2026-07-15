// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `#[repr(C)]` POD projection for the OPAQUE_FD / CUDA buffer surface
//! (#1262).
//!
//! Pure-POD (primitives + a byte array), so its layout is fully
//! source-determined. Locked by the per-struct `offset_of!` regression
//! test here and deliberately excluded from
//! [`crate::PLUGIN_ABI_LAYOUT_FINGERPRINT`] (the POD exclusion rule —
//! the `GpuCapabilitiesRepr` precedent).

/// Descriptor written by the `export_storage_buffer_opaque_fd`
/// FullAccess slot: a fresh dup'd kernel fd plus the exporting device's
/// UUID, so a cdylib-resident CUDA adapter can
/// `cudaImportExternalMemory` the fd and bind its CUDA context to the
/// matching physical device — with no host Vulkan device crossing the
/// ABI.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OpaqueFdExportDescriptorRepr {
    /// Fresh `OPAQUE_FD` from `vkGetMemoryFdKHR`; caller owns it.
    /// Written `-1` on any non-zero return (the failure path never
    /// leaves a stale live fd, preventing a double-close).
    pub fd: i32,
    /// `VkExternalMemoryHandleTypeFlagBits` (`OPAQUE_FD = 0x00000001`);
    /// self-describing → also fills the alignment gap ahead of `size`
    /// and permits a future DMA_BUF-export reuse.
    pub handle_type_raw: u32,
    /// Allocation byte size (`== byte_size`).
    pub size: u64,
    /// `VkPhysicalDeviceIDProperties::deviceUUID` of the exporting
    /// device — the entire CUDA device-binding contract (a mismatched
    /// CUDA device fails with a typed error, never silent fall-through
    /// to CUDA device 0).
    pub device_uuid: [u8; 16],
}

impl Default for OpaqueFdExportDescriptorRepr {
    fn default() -> Self {
        // `-1` fd is the "no live fd" sentinel; matches the failure-path
        // contract so a zero-init landing slot never reads as fd 0.
        Self {
            fd: -1,
            handle_type_raw: 0,
            size: 0,
            device_uuid: [0u8; 16],
        }
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn opaque_fd_export_descriptor_repr_layout() {
        assert_eq!(size_of::<OpaqueFdExportDescriptorRepr>(), 32);
        assert_eq!(align_of::<OpaqueFdExportDescriptorRepr>(), 8);
        assert_eq!(offset_of!(OpaqueFdExportDescriptorRepr, fd), 0);
        assert_eq!(offset_of!(OpaqueFdExportDescriptorRepr, handle_type_raw), 4);
        assert_eq!(offset_of!(OpaqueFdExportDescriptorRepr, size), 8);
        assert_eq!(offset_of!(OpaqueFdExportDescriptorRepr, device_uuid), 16);
    }

    #[test]
    fn default_fd_is_minus_one() {
        assert_eq!(OpaqueFdExportDescriptorRepr::default().fd, -1);
    }
}
