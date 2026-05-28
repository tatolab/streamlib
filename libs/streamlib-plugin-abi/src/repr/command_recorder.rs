// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `#[repr(C)]` helper structs used by [`crate::RhiCommandRecorderMethodsVTable`]
//! and [`crate::RhiColorConverterMethodsVTable`] dispatch — buffer↔image copy
//! regions, semaphore-submit info, source/output color-conversion layout.

/// `#[repr(C)]` mirror of `streamlib::core::rhi::color_converter::SourceLayoutInfo`.
///
/// Plane strides + UV-plane offset for the buffer→image kernel's
/// SSBO walk. Layout-locked by the regression test in `layout_tests`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SourceLayoutInfoRepr {
    /// Y plane (NV12) or packed plane (YUYV) row stride in bytes.
    pub plane0_stride_bytes: u32,
    /// UV plane row stride in bytes for NV12; zero for YUYV.
    pub plane1_stride_bytes: u32,
    /// Offset of the UV plane from the start of the source SSBO,
    /// in bytes. Zero for YUYV (single plane).
    pub plane1_offset_bytes: u32,
    /// Reserved padding so the struct stays 4-byte-multiple sized
    /// and naturally aligned; zero today, never read.
    pub _reserved_padding: u32,
}

/// `#[repr(C)]` mirror of
/// `streamlib::vulkan::rhi::vulkan_command_recorder::ImageCopyRegion`.
///
/// Buffer↔image copy region — single mip level, single array layer,
/// color aspect, full image. Used by
/// [`RhiCommandRecorderMethodsVTable::record_copy_image_to_buffer`]
/// to cross the cdylib boundary without dragging callers through
/// `vulkanalia` imports. Layout-locked by the regression test in
/// `layout_tests`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageCopyRegionRepr {
    /// Image extent width in pixels.
    pub width: u32,
    /// Image extent height in pixels.
    pub height: u32,
    /// Byte offset within the buffer where the copy begins.
    pub buffer_offset: u64,
    /// Buffer row length in pixels (matches image width for tightly
    /// packed copies).
    pub buffer_row_length: u32,
    /// Buffer image height in pixels (matches image height for
    /// tightly packed copies).
    pub buffer_image_height: u32,
    /// Mip level to copy into / out of.
    pub mip_level: u32,
    /// Array layer to copy into / out of.
    pub array_layer: u32,
    /// Reserved padding so the struct stays 8-byte-aligned and the
    /// trailing bytes of the last 4-byte field are deterministic;
    /// zero today, never read.
    pub _reserved_padding: u32,
}

/// `#[repr(C)]` mirror of [`vk::SemaphoreSubmitInfo`]'s
/// engine-relevant fields, for the v3 `submit_with_semaphores`
/// vtable slot. Layout-locked by the regression test in
/// `layout_tests`.
///
/// The host re-materializes a `vk::SemaphoreSubmitInfo` from these
/// fields on the host side at the call site; the ABI doesn't pull
/// `vulkanalia-sys` into its dep graph.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SemaphoreSubmitInfoRepr {
    /// Raw `VkSemaphore` handle (widened to `u64`). Binary or
    /// timeline; the host doesn't need to know which here — the
    /// `value` field is `0` for binary semaphores by convention.
    pub semaphore: u64,
    /// Wait/signal value for timeline semaphores; `0` for binary
    /// semaphores.
    pub value: u64,
    /// `VkPipelineStageFlags2` bitmask (Vulkan 1.3 / synchronization2).
    pub stage_mask: u64,
    /// Device index for multi-device submits; `0` for single-device
    /// (the only case streamlib supports today).
    pub device_index: u32,
    /// Reserved padding so the struct stays 8-byte-aligned and the
    /// trailing bytes are deterministic; zero today, never read.
    pub _reserved_padding: u32,
}


/// `#[repr(C)]` mirror of `streamlib::core::color::ResolvedColorInfo`.
///
/// Each field is the matching engine-side `#[repr(u32)]` enum's
/// discriminant: `primaries_raw` mirrors `PrimariesId`, `transfer_raw`
/// mirrors `TransferId`, `matrix_raw` mirrors `MatrixId`, `range_raw`
/// mirrors `RangeId`. Layout-locked by the regression test in
/// `layout_tests`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ResolvedColorInfoRepr {
    /// `PrimariesId` discriminant.
    pub primaries_raw: u32,
    /// `TransferId` discriminant.
    pub transfer_raw: u32,
    /// `MatrixId` discriminant.
    pub matrix_raw: u32,
    /// `RangeId` discriminant.
    pub range_raw: u32,
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn source_layout_info_repr_layout() {
        // Four u32 fields = 16 bytes, align 4.
        assert_eq!(size_of::<SourceLayoutInfoRepr>(), 16);
        assert_eq!(align_of::<SourceLayoutInfoRepr>(), 4);
        assert_eq!(offset_of!(SourceLayoutInfoRepr, plane0_stride_bytes), 0);
        assert_eq!(offset_of!(SourceLayoutInfoRepr, plane1_stride_bytes), 4);
        assert_eq!(offset_of!(SourceLayoutInfoRepr, plane1_offset_bytes), 8);
        assert_eq!(offset_of!(SourceLayoutInfoRepr, _reserved_padding), 12);
    }

    #[test]
    fn resolved_color_info_repr_layout() {
        // Four u32 discriminants = 16 bytes, align 4.
        assert_eq!(size_of::<ResolvedColorInfoRepr>(), 16);
        assert_eq!(align_of::<ResolvedColorInfoRepr>(), 4);
        assert_eq!(offset_of!(ResolvedColorInfoRepr, primaries_raw), 0);
        assert_eq!(offset_of!(ResolvedColorInfoRepr, transfer_raw), 4);
        assert_eq!(offset_of!(ResolvedColorInfoRepr, matrix_raw), 8);
        assert_eq!(offset_of!(ResolvedColorInfoRepr, range_raw), 12);
    }

    #[test]
    fn image_copy_region_repr_layout() {
        // Field layout:
        //   width                @ 0   (4 bytes, u32)
        //   height               @ 4   (4 bytes, u32)
        //   buffer_offset        @ 8   (8 bytes, u64)
        //   buffer_row_length    @ 16  (4 bytes, u32)
        //   buffer_image_height  @ 20  (4 bytes, u32)
        //   mip_level            @ 24  (4 bytes, u32)
        //   array_layer          @ 28  (4 bytes, u32)
        //   _reserved_padding    @ 32  (4 bytes, u32)
        // Total = 40 bytes with 4-byte tail padding rounded up to
        // align(8) = 40 bytes. The struct's alignment is 8 because
        // of the `u64` field.
        assert_eq!(size_of::<ImageCopyRegionRepr>(), 40);
        assert_eq!(align_of::<ImageCopyRegionRepr>(), 8);
        assert_eq!(offset_of!(ImageCopyRegionRepr, width), 0);
        assert_eq!(offset_of!(ImageCopyRegionRepr, height), 4);
        assert_eq!(offset_of!(ImageCopyRegionRepr, buffer_offset), 8);
        assert_eq!(offset_of!(ImageCopyRegionRepr, buffer_row_length), 16);
        assert_eq!(offset_of!(ImageCopyRegionRepr, buffer_image_height), 20);
        assert_eq!(offset_of!(ImageCopyRegionRepr, mip_level), 24);
        assert_eq!(offset_of!(ImageCopyRegionRepr, array_layer), 28);
        assert_eq!(offset_of!(ImageCopyRegionRepr, _reserved_padding), 32);
    }

    #[test]
    fn semaphore_submit_info_repr_layout() {
        // semaphore (u64, 8) + value (u64, 8) + stage_mask (u64, 8)
        // + device_index (u32, 4) + _reserved_padding (u32, 4) = 32
        assert_eq!(size_of::<SemaphoreSubmitInfoRepr>(), 32);
        assert_eq!(align_of::<SemaphoreSubmitInfoRepr>(), 8);
        assert_eq!(offset_of!(SemaphoreSubmitInfoRepr, semaphore), 0);
        assert_eq!(offset_of!(SemaphoreSubmitInfoRepr, value), 8);
        assert_eq!(offset_of!(SemaphoreSubmitInfoRepr, stage_mask), 16);
        assert_eq!(offset_of!(SemaphoreSubmitInfoRepr, device_index), 24);
        assert_eq!(
            offset_of!(SemaphoreSubmitInfoRepr, _reserved_padding),
            28
        );
    }
}
