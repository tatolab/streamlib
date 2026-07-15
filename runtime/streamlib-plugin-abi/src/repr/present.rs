// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `#[repr(C)]` POD projections for the present-target surface (#1258).
//!
//! Pure-POD wire types the present-target `create_present_target` slot
//! and the [`crate::PresentTargetMethodsVTable`] method slots consume.
//! Their byte layout is fully source-determined, so they are locked by
//! the per-struct `offset_of!` regression tests here and deliberately
//! excluded from [`crate::PLUGIN_ABI_LAYOUT_FINGERPRINT`] (the POD
//! exclusion rule — see the fold doc-comment in `lib.rs`).

/// Flattened tagged-union projection of `raw-window-handle`'s
/// non-exhaustive `RawWindowHandle` / `RawDisplayHandle` enums.
///
/// Linux-complete: `Xlib` / `Xcb` / `Wayland` discriminants carry live
/// data. The `Win32` and `AppKit` discriminants (plus their payload
/// fields) are reserved from day one so Windows / macOS activation
/// lands only a new host dispatch arm, never a layout-version bump.
/// Irrelevant fields for a given `kind` are zero.
///
/// The caller (SDK / winit event loop) owns the native window and must
/// outlive the minted `PresentTarget`; the host creates the
/// `VkSurfaceKHR` and from that point owns the surface, never the
/// window.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RawWindowHandleRepr {
    /// Window-handle kind discriminant: `0 = Xlib`, `1 = Xcb`,
    /// `2 = Wayland`, `3 = Win32` (RESERVED), `4 = AppKit` (RESERVED).
    pub kind: u32,
    /// Reserved padding (keeps the following `u64` naturally aligned;
    /// zero today, never read).
    pub _reserved_padding: u32,
    /// Xlib `Window` / Xcb `window` / Wayland `wl_surface*` / Win32
    /// `HWND` / AppKit `NSView*` (widened to `u64`).
    pub window_or_surface: u64,
    /// Xlib `Display*` / Xcb `xcb_connection_t*` / Wayland `wl_display*`
    /// / Win32 `HINSTANCE` / AppKit `0` (widened to `u64`).
    pub display_or_connection: u64,
    /// Xlib / Xcb screen index; `0` elsewhere.
    pub screen: u32,
    /// Reserved tail (absorbs an Xcb `visual_id` later without a layout
    /// change; zero today, never read).
    pub _reserved_tail: u32,
}

/// `begin_frame` out-struct: the per-frame state the host hands back
/// after acquiring the next swapchain image.
///
/// `recorder_handle` is a **borrowed, non-owning** raw pointer to the
/// present target's internal per-frame recorder — the caller drives it
/// through the already-shipped [`crate::RhiCommandRecorderMethodsVTable`]
/// slots and must NEVER release it (that would double-free the present
/// target's own recorder). Ownership stays with the `PresentTarget`
/// across the begin/end split.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PresentFrameBeginRepr {
    /// Borrowed raw recorder handle; `0` when `acquired_ok == 0`.
    /// NON-OWNING — do not release.
    pub recorder_handle: u64,
    /// `VkImage` of the acquired swapchain image (widened to `u64`).
    pub image_raw: u64,
    /// `VkImageView` feeding `cmd_begin_dynamic_rendering` (widened to
    /// `u64`).
    pub image_view_raw: u64,
    /// Frame index within the swapchain's per-image ring.
    pub frame_index: u32,
    /// Acquired image width.
    pub extent_w: u32,
    /// Acquired image height.
    pub extent_h: u32,
    /// `1` = image acquired; `0` = `OUT_OF_DATE_KHR` (drive `recreate`,
    /// do NOT call `end_frame`).
    pub acquired_ok: u32,
    /// Live `TextureFormat` discriminant of the swapchain image (never a
    /// stale cached copy) for kernel attachment matching.
    pub color_format_raw: u32,
    /// Reserved padding (keeps the struct a multiple of 8; zero today,
    /// never read).
    pub _reserved_padding: u32,
}

/// 2-axis optional color-traits projection of `ColorTraits`
/// (`{primaries: Option<PrimariesId>, transfer: Option<TransferId>}`).
///
/// Passed by `*const ColorTraitsRepr`; a null pointer = whole-struct
/// None = legacy SDR pick. Distinct from `ResolvedColorInfoRepr` (which
/// mirrors the fully-resolved 4-axis `ResolvedColorInfo`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ColorTraitsRepr {
    /// `PrimariesId` discriminant; `u32::MAX` = None.
    pub primaries_raw: u32,
    /// `TransferId` discriminant; `u32::MAX` = None.
    pub transfer_raw: u32,
}

/// Projection of `HdrStaticMetadata` for `set_hdr_metadata`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HdrStaticMetadataRepr {
    /// Display primary red chromaticity `[x, y]`.
    pub display_primary_red: [f32; 2],
    /// Display primary green chromaticity `[x, y]`.
    pub display_primary_green: [f32; 2],
    /// Display primary blue chromaticity `[x, y]`.
    pub display_primary_blue: [f32; 2],
    /// White point chromaticity `[x, y]`.
    pub white_point: [f32; 2],
    /// Minimum display luminance (cd/m²).
    pub min_luminance_cd_m2: f32,
    /// Maximum display luminance (cd/m²).
    pub max_luminance_cd_m2: f32,
    /// Maximum content light level (cd/m²).
    pub max_content_light_level: f32,
    /// Maximum frame-average light level (cd/m²).
    pub max_frame_average_light_level: f32,
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn raw_window_handle_repr_layout() {
        assert_eq!(size_of::<RawWindowHandleRepr>(), 32);
        assert_eq!(align_of::<RawWindowHandleRepr>(), 8);
        assert_eq!(offset_of!(RawWindowHandleRepr, kind), 0);
        assert_eq!(offset_of!(RawWindowHandleRepr, _reserved_padding), 4);
        assert_eq!(offset_of!(RawWindowHandleRepr, window_or_surface), 8);
        assert_eq!(offset_of!(RawWindowHandleRepr, display_or_connection), 16);
        assert_eq!(offset_of!(RawWindowHandleRepr, screen), 24);
        assert_eq!(offset_of!(RawWindowHandleRepr, _reserved_tail), 28);
    }

    #[test]
    fn present_frame_begin_repr_layout() {
        assert_eq!(size_of::<PresentFrameBeginRepr>(), 48);
        assert_eq!(align_of::<PresentFrameBeginRepr>(), 8);
        assert_eq!(offset_of!(PresentFrameBeginRepr, recorder_handle), 0);
        assert_eq!(offset_of!(PresentFrameBeginRepr, image_raw), 8);
        assert_eq!(offset_of!(PresentFrameBeginRepr, image_view_raw), 16);
        assert_eq!(offset_of!(PresentFrameBeginRepr, frame_index), 24);
        assert_eq!(offset_of!(PresentFrameBeginRepr, extent_w), 28);
        assert_eq!(offset_of!(PresentFrameBeginRepr, extent_h), 32);
        assert_eq!(offset_of!(PresentFrameBeginRepr, acquired_ok), 36);
        assert_eq!(offset_of!(PresentFrameBeginRepr, color_format_raw), 40);
        assert_eq!(offset_of!(PresentFrameBeginRepr, _reserved_padding), 44);
    }

    #[test]
    fn color_traits_repr_layout() {
        assert_eq!(size_of::<ColorTraitsRepr>(), 8);
        assert_eq!(align_of::<ColorTraitsRepr>(), 4);
        assert_eq!(offset_of!(ColorTraitsRepr, primaries_raw), 0);
        assert_eq!(offset_of!(ColorTraitsRepr, transfer_raw), 4);
    }

    #[test]
    fn hdr_static_metadata_repr_layout() {
        assert_eq!(size_of::<HdrStaticMetadataRepr>(), 48);
        assert_eq!(align_of::<HdrStaticMetadataRepr>(), 4);
        assert_eq!(offset_of!(HdrStaticMetadataRepr, display_primary_red), 0);
        assert_eq!(offset_of!(HdrStaticMetadataRepr, display_primary_green), 8);
        assert_eq!(offset_of!(HdrStaticMetadataRepr, display_primary_blue), 16);
        assert_eq!(offset_of!(HdrStaticMetadataRepr, white_point), 24);
        assert_eq!(offset_of!(HdrStaticMetadataRepr, min_luminance_cd_m2), 32);
        assert_eq!(offset_of!(HdrStaticMetadataRepr, max_luminance_cd_m2), 36);
        assert_eq!(offset_of!(HdrStaticMetadataRepr, max_content_light_level), 40);
        assert_eq!(
            offset_of!(HdrStaticMetadataRepr, max_frame_average_light_level),
            44
        );
    }
}
