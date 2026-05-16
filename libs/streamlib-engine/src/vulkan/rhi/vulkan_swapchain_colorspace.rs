// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Swapchain colorspace negotiation + HDR static metadata materialization.

use vulkanalia::vk;
use vulkanalia::vk::HasBuilder as _;

use crate::_generated_::tatolab__core::color_info::{Primaries, Transfer};
use crate::_generated_::{ColorInfo, ContentLight, MasteringDisplay};

/// Result of [`pick_swapchain_format`] — the chosen `(format, color_space)`
/// pair plus a flag indicating whether the chosen colorspace expects HDR
/// signaling (PQ or HLG variants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwapchainColorPick {
    pub format: vk::Format,
    pub color_space: vk::ColorSpaceKHR,
    /// True when the picked colorspace is one of the HDR signaling
    /// variants. Drives whether [`build_hdr_metadata`] should be
    /// materialized + attached via `vkSetHdrMetadataEXT`.
    pub is_hdr: bool,
}

/// Walks the surface-exposed format list and picks the best
/// `(VkFormat, VkColorSpaceKHR)` for the incoming frame's color
/// description. SRGB_NONLINEAR + BGRA8_UNORM (then anything the
/// surface offers) is the universal fallback.
///
/// Priority walk:
/// 1. Frame says PQ + BT.2020 (HDR10) → walk for `HDR10_ST2084_EXT`,
///    preferring `A2B10G10R10_UNORM_PACK32` then `R16G16B16A16_SFLOAT`.
/// 2. Frame says HLG + BT.2020 → walk for `HDR10_HLG_EXT` with the
///    same format preferences.
/// 3. HDR signal but no matching HDR10 colorspace exposed → walk for
///    `EXTENDED_SRGB_LINEAR_EXT` + `R16G16B16A16_SFLOAT` (scRGB-style
///    float scanout — Mesa Wayland-only). Engine tone-maps to display.
/// 4. SDR signal or fallthrough → `SRGB_NONLINEAR_KHR` with
///    `B8G8R8A8_UNORM` if exposed, otherwise the first format the
///    surface offered. Engine tone-maps + gamut-maps as needed.
pub fn pick_swapchain_format(
    surface_formats: &[vk::SurfaceFormatKHR],
    color_info: Option<&ColorInfo>,
) -> SwapchainColorPick {
    debug_assert!(
        !surface_formats.is_empty(),
        "vkGetPhysicalDeviceSurfaceFormatsKHR returned an empty list — \
         spec requires at least one entry on a supported surface"
    );

    let want_pq = matches!(color_info.and_then(|c| c.transfer.as_ref()), Some(Transfer::Smpte2084));
    let want_hlg =
        matches!(color_info.and_then(|c| c.transfer.as_ref()), Some(Transfer::AribStdB67));
    let want_bt2020 =
        matches!(color_info.and_then(|c| c.primaries.as_ref()), Some(Primaries::Bt2020));

    if want_pq && want_bt2020 {
        if let Some(pick) = walk_hdr10(surface_formats, vk::ColorSpaceKHR::HDR10_ST2084_EXT) {
            return pick;
        }
    } else if want_hlg && want_bt2020 {
        if let Some(pick) = walk_hdr10(surface_formats, vk::ColorSpaceKHR::HDR10_HLG_EXT) {
            return pick;
        }
    }

    // HDR signal present but no HDR10 colorspace exposed — try scRGB-style
    // float scanout. Mesa exposes this on Wayland; NVIDIA does not. Engine
    // is responsible for tone-mapping to the display luminance range.
    if (want_pq || want_hlg)
        && let Some(pick) = walk_extended_srgb_linear(surface_formats)
    {
        return pick;
    }

    // SDR / fallthrough: SRGB_NONLINEAR with BGRA8_UNORM, else BGRA8_SRGB,
    // else the first format the surface advertised. Matches the legacy
    // hardcoded pick so SDR-on-SDR behaves identically to today.
    pick_srgb_fallback(surface_formats)
}

fn walk_hdr10(
    surface_formats: &[vk::SurfaceFormatKHR],
    target: vk::ColorSpaceKHR,
) -> Option<SwapchainColorPick> {
    // Prefer the 10-bit packed format — that's the canonical HDR10
    // wire format. Fall back to FP16 if the driver only exposes
    // float-scanout for the HDR10 colorspace (rare).
    const HDR10_FORMATS: &[vk::Format] = &[
        vk::Format::A2B10G10R10_UNORM_PACK32,
        vk::Format::A2R10G10B10_UNORM_PACK32,
        vk::Format::R16G16B16A16_SFLOAT,
    ];
    for &want_format in HDR10_FORMATS {
        if let Some(sf) = surface_formats
            .iter()
            .find(|sf| sf.format == want_format && sf.color_space == target)
        {
            return Some(SwapchainColorPick {
                format: sf.format,
                color_space: sf.color_space,
                is_hdr: true,
            });
        }
    }
    None
}

fn walk_extended_srgb_linear(
    surface_formats: &[vk::SurfaceFormatKHR],
) -> Option<SwapchainColorPick> {
    // scRGB scanout requires float — half-float is the canonical
    // choice. EXTENDED_SRGB_NONLINEAR pairs with 8-bit but is not
    // wide-gamut in the same way; we only walk the linear variant
    // here since we're explicitly looking for an HDR-capable scanout.
    let target = vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT;
    surface_formats
        .iter()
        .find(|sf| sf.format == vk::Format::R16G16B16A16_SFLOAT && sf.color_space == target)
        .map(|sf| SwapchainColorPick {
            format: sf.format,
            color_space: sf.color_space,
            // Float scanout signals HDR-capable but is not a PQ/HLG
            // colorspace; vkSetHdrMetadataEXT is documented as
            // applicable only to HDR10/HDR10_HLG, so leave is_hdr
            // false here. The engine still does the tone-mapping.
            is_hdr: false,
        })
}

fn pick_srgb_fallback(surface_formats: &[vk::SurfaceFormatKHR]) -> SwapchainColorPick {
    // Match the legacy pick first (BGRA8_UNORM + SRGB_NONLINEAR) so
    // SDR-on-SDR is byte-identical to today's behavior. Then BGRA8_SRGB
    // (some compositors only offer the sRGB-encoded variant). Last
    // resort: surface_formats[0], whatever it is.
    const SRGB_PRIORITIES: &[(vk::Format, vk::ColorSpaceKHR)] = &[
        (vk::Format::B8G8R8A8_UNORM, vk::ColorSpaceKHR::SRGB_NONLINEAR),
        (vk::Format::B8G8R8A8_SRGB, vk::ColorSpaceKHR::SRGB_NONLINEAR),
        (vk::Format::R8G8B8A8_UNORM, vk::ColorSpaceKHR::SRGB_NONLINEAR),
        (vk::Format::R8G8B8A8_SRGB, vk::ColorSpaceKHR::SRGB_NONLINEAR),
    ];
    for &(want_format, want_color_space) in SRGB_PRIORITIES {
        if let Some(sf) = surface_formats
            .iter()
            .find(|sf| sf.format == want_format && sf.color_space == want_color_space)
        {
            return SwapchainColorPick {
                format: sf.format,
                color_space: sf.color_space,
                is_hdr: false,
            };
        }
    }
    let sf = surface_formats[0];
    SwapchainColorPick {
        format: sf.format,
        color_space: sf.color_space,
        is_hdr: false,
    }
}

/// Materializes a `vk::HdrMetadataEXT` from the H.265 SEI / MP4 mdcv
/// wire-format integers carried in the schemas. Schema units:
///
/// - chromaticity in 1/50000 increments (CIE 1931) → divided by 50000
///   to land in `XYColorEXT`'s `[0.0, 1.0]` float range.
/// - luminance in 0.0001 cd/m² increments → divided by 10000 to land
///   in `HdrMetadataEXT`'s cd/m² float range.
/// - `max_cll` / `max_fall` are integer cd/m² in the schema → cast to
///   f32 directly (no scaling).
pub fn build_hdr_metadata(
    mastering: &MasteringDisplay,
    content_light: &ContentLight,
) -> vk::HdrMetadataEXT {
    const CHROMA_SCALE: f32 = 1.0 / 50_000.0;
    const LUM_SCALE: f32 = 1.0 / 10_000.0;

    vk::HdrMetadataEXT::builder()
        .display_primary_red(vk::XYColorEXT {
            x: mastering.display_primaries_r_x as f32 * CHROMA_SCALE,
            y: mastering.display_primaries_r_y as f32 * CHROMA_SCALE,
        })
        .display_primary_green(vk::XYColorEXT {
            x: mastering.display_primaries_g_x as f32 * CHROMA_SCALE,
            y: mastering.display_primaries_g_y as f32 * CHROMA_SCALE,
        })
        .display_primary_blue(vk::XYColorEXT {
            x: mastering.display_primaries_b_x as f32 * CHROMA_SCALE,
            y: mastering.display_primaries_b_y as f32 * CHROMA_SCALE,
        })
        .white_point(vk::XYColorEXT {
            x: mastering.white_point_x as f32 * CHROMA_SCALE,
            y: mastering.white_point_y as f32 * CHROMA_SCALE,
        })
        .max_luminance(mastering.max_luminance as f32 * LUM_SCALE)
        .min_luminance(mastering.min_luminance as f32 * LUM_SCALE)
        .max_content_light_level(content_light.max_cll as f32)
        .max_frame_average_light_level(content_light.max_fall as f32)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(format: vk::Format, color_space: vk::ColorSpaceKHR) -> vk::SurfaceFormatKHR {
        vk::SurfaceFormatKHR { format, color_space }
    }

    fn pq_bt2020() -> ColorInfo {
        ColorInfo {
            primaries: Some(Primaries::Bt2020),
            transfer: Some(Transfer::Smpte2084),
            matrix: None,
            range: None,
        }
    }

    fn hlg_bt2020() -> ColorInfo {
        ColorInfo {
            primaries: Some(Primaries::Bt2020),
            transfer: Some(Transfer::AribStdB67),
            matrix: None,
            range: None,
        }
    }

    fn srgb_only() -> Vec<vk::SurfaceFormatKHR> {
        vec![
            fmt(vk::Format::B8G8R8A8_UNORM, vk::ColorSpaceKHR::SRGB_NONLINEAR),
            fmt(vk::Format::B8G8R8A8_SRGB, vk::ColorSpaceKHR::SRGB_NONLINEAR),
        ]
    }

    fn nvidia_hdr10() -> Vec<vk::SurfaceFormatKHR> {
        vec![
            fmt(vk::Format::B8G8R8A8_UNORM, vk::ColorSpaceKHR::SRGB_NONLINEAR),
            fmt(vk::Format::B8G8R8A8_SRGB, vk::ColorSpaceKHR::SRGB_NONLINEAR),
            fmt(
                vk::Format::A2B10G10R10_UNORM_PACK32,
                vk::ColorSpaceKHR::HDR10_ST2084_EXT,
            ),
        ]
    }

    fn mesa_full_set() -> Vec<vk::SurfaceFormatKHR> {
        vec![
            fmt(vk::Format::B8G8R8A8_UNORM, vk::ColorSpaceKHR::SRGB_NONLINEAR),
            fmt(vk::Format::B8G8R8A8_SRGB, vk::ColorSpaceKHR::SRGB_NONLINEAR),
            fmt(
                vk::Format::A2B10G10R10_UNORM_PACK32,
                vk::ColorSpaceKHR::HDR10_ST2084_EXT,
            ),
            fmt(
                vk::Format::A2B10G10R10_UNORM_PACK32,
                vk::ColorSpaceKHR::HDR10_HLG_EXT,
            ),
            fmt(
                vk::Format::R16G16B16A16_SFLOAT,
                vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT,
            ),
        ]
    }

    /// SDR-only surfaces (today's NVIDIA X11 reality) must always pick
    /// the legacy `BGRA8_UNORM + SRGB_NONLINEAR` pair, regardless of
    /// what the frame's `ColorInfo` requests. Mentally reverting the
    /// `pick_srgb_fallback` priority order to "first format wins" would
    /// regress to a non-deterministic pick; this test catches that.
    #[test]
    fn sdr_only_surface_always_picks_bgra8_srgb_nonlinear() {
        let formats = srgb_only();
        for color_info in [None, Some(pq_bt2020()), Some(hlg_bt2020())] {
            let pick = pick_swapchain_format(&formats, color_info.as_ref());
            assert_eq!(pick.format, vk::Format::B8G8R8A8_UNORM);
            assert_eq!(pick.color_space, vk::ColorSpaceKHR::SRGB_NONLINEAR);
            assert!(!pick.is_hdr);
        }
    }

    /// PQ + BT.2020 frame against an HDR10-capable surface must pick
    /// the 10-bit packed HDR10 pair and signal `is_hdr=true`. Mentally
    /// reverting the `walk_hdr10` priority list to start at FP16 would
    /// pick a needlessly-wide format; this asserts the canonical HDR10
    /// 10-bit packed shape.
    #[test]
    fn pq_bt2020_picks_hdr10_st2084_with_a2b10g10r10() {
        let pick = pick_swapchain_format(&nvidia_hdr10(), Some(&pq_bt2020()));
        assert_eq!(pick.format, vk::Format::A2B10G10R10_UNORM_PACK32);
        assert_eq!(pick.color_space, vk::ColorSpaceKHR::HDR10_ST2084_EXT);
        assert!(pick.is_hdr);
    }

    /// HLG + BT.2020 frame against a Mesa-shaped full set must pick
    /// HDR10_HLG_EXT, not HDR10_ST2084_EXT. Mentally reverting the
    /// HLG arm to fall through to the PQ arm would mis-signal HLG
    /// content as PQ.
    #[test]
    fn hlg_bt2020_picks_hdr10_hlg() {
        let pick = pick_swapchain_format(&mesa_full_set(), Some(&hlg_bt2020()));
        assert_eq!(pick.format, vk::Format::A2B10G10R10_UNORM_PACK32);
        assert_eq!(pick.color_space, vk::ColorSpaceKHR::HDR10_HLG_EXT);
        assert!(pick.is_hdr);
    }

    /// HDR-signaled frame against a surface that exposes only
    /// scRGB-style float scanout (no HDR10 colorspace) must pick the
    /// `EXTENDED_SRGB_LINEAR_EXT` + FP16 pair. Mentally reverting the
    /// scRGB fallback would force this case down to SRGB_NONLINEAR
    /// and lose the HDR-capable scanout the surface offered.
    #[test]
    fn hdr_signal_falls_through_to_extended_srgb_linear_when_no_hdr10() {
        let formats = vec![
            fmt(vk::Format::B8G8R8A8_UNORM, vk::ColorSpaceKHR::SRGB_NONLINEAR),
            fmt(
                vk::Format::R16G16B16A16_SFLOAT,
                vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT,
            ),
        ];
        let pick = pick_swapchain_format(&formats, Some(&pq_bt2020()));
        assert_eq!(pick.format, vk::Format::R16G16B16A16_SFLOAT);
        assert_eq!(
            pick.color_space,
            vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT
        );
        // Float scanout doesn't take vkSetHdrMetadataEXT — the
        // colorspace isn't PQ/HLG, the engine handles tone-mapping
        // and the metadata would be a no-op signal.
        assert!(!pick.is_hdr);
    }

    /// PQ frame against an HDR10 surface where only FP16 is exposed
    /// for the HDR10 colorspace (some non-stock drivers) must still
    /// pick HDR10_ST2084_EXT — the colorspace is what matters, not
    /// the bit depth.
    #[test]
    fn hdr10_picks_fp16_when_packed_10_bit_unavailable() {
        let formats = vec![
            fmt(vk::Format::B8G8R8A8_UNORM, vk::ColorSpaceKHR::SRGB_NONLINEAR),
            fmt(
                vk::Format::R16G16B16A16_SFLOAT,
                vk::ColorSpaceKHR::HDR10_ST2084_EXT,
            ),
        ];
        let pick = pick_swapchain_format(&formats, Some(&pq_bt2020()));
        assert_eq!(pick.format, vk::Format::R16G16B16A16_SFLOAT);
        assert_eq!(pick.color_space, vk::ColorSpaceKHR::HDR10_ST2084_EXT);
        assert!(pick.is_hdr);
    }

    /// `None` ColorInfo + HDR-capable surface must NOT promote the
    /// pick to HDR10. Default-frame producers rely on the SRGB
    /// fallback to stay byte-identical to today's behavior, and
    /// promoting an absent ColorInfo to HDR would silently change
    /// the engine's color-handling for every existing pipeline.
    #[test]
    fn absent_color_info_against_hdr_surface_stays_srgb() {
        let pick = pick_swapchain_format(&mesa_full_set(), None);
        assert_eq!(pick.format, vk::Format::B8G8R8A8_UNORM);
        assert_eq!(pick.color_space, vk::ColorSpaceKHR::SRGB_NONLINEAR);
        assert!(!pick.is_hdr);
    }

    /// `Smpte2084` (PQ) without `Bt2020` primaries is technically a
    /// non-standard combination — HDR10 is defined as PQ + BT.2020.
    /// Picking HDR10 anyway would mis-signal the scanout primaries.
    /// Falling through to SRGB is correct: the engine tone-maps PQ
    /// down to SDR, and the wrong-primaries case can't be fixed by
    /// the colorspace pick.
    #[test]
    fn pq_without_bt2020_does_not_pick_hdr10() {
        let color_info = ColorInfo {
            primaries: Some(Primaries::Bt709),
            transfer: Some(Transfer::Smpte2084),
            matrix: None,
            range: None,
        };
        let pick = pick_swapchain_format(&nvidia_hdr10(), Some(&color_info));
        assert_eq!(pick.format, vk::Format::B8G8R8A8_UNORM);
        assert_eq!(pick.color_space, vk::ColorSpaceKHR::SRGB_NONLINEAR);
        assert!(!pick.is_hdr);
    }

    /// `build_hdr_metadata` must convert the schema's wire-format
    /// integers to the `vk::HdrMetadataEXT` floating-point fields
    /// using the right scale per axis: 1/50000 for chromaticity,
    /// 1/10000 for mastering luminance, 1.0 for content light. A
    /// silent bug here ships wrong HDR metadata to the driver — the
    /// scanout will tone-map against the wrong volume.
    ///
    /// Reference values: BT.2020 primaries + D65 white point + a
    /// canonical HDR10 mastering display (1000 cd/m² peak,
    /// 0.0001 cd/m² floor).
    #[test]
    fn hdr_metadata_round_trip_uses_correct_unit_scaling() {
        let mastering = MasteringDisplay {
            // BT.2020 primaries:  R = (0.708, 0.292)  → 35400, 14600
            //                     G = (0.170, 0.797)  →  8500, 39850
            //                     B = (0.131, 0.046)  →  6550,  2300
            display_primaries_r_x: 35_400,
            display_primaries_r_y: 14_600,
            display_primaries_g_x: 8_500,
            display_primaries_g_y: 39_850,
            display_primaries_b_x: 6_550,
            display_primaries_b_y: 2_300,
            // D65 white point: (0.3127, 0.3290) → 15635, 16450
            white_point_x: 15_635,
            white_point_y: 16_450,
            // 1000 cd/m² peak → 10_000_000 in 0.0001 cd/m² units.
            max_luminance: 10_000_000,
            // 0.005 cd/m² floor → 50.
            min_luminance: 50,
        };
        let content_light = ContentLight {
            max_cll: 1000,
            max_fall: 400,
        };

        let md = build_hdr_metadata(&mastering, &content_light);

        let eps = 1e-4;
        assert!((md.display_primary_red.x - 0.708).abs() < eps, "red.x={}", md.display_primary_red.x);
        assert!((md.display_primary_red.y - 0.292).abs() < eps, "red.y={}", md.display_primary_red.y);
        assert!((md.display_primary_green.x - 0.170).abs() < eps, "green.x={}", md.display_primary_green.x);
        assert!((md.display_primary_green.y - 0.797).abs() < eps, "green.y={}", md.display_primary_green.y);
        assert!((md.display_primary_blue.x - 0.131).abs() < eps, "blue.x={}", md.display_primary_blue.x);
        assert!((md.display_primary_blue.y - 0.046).abs() < eps, "blue.y={}", md.display_primary_blue.y);
        assert!((md.white_point.x - 0.3127).abs() < eps, "white.x={}", md.white_point.x);
        assert!((md.white_point.y - 0.3290).abs() < eps, "white.y={}", md.white_point.y);
        assert!((md.max_luminance - 1000.0).abs() < 1e-3, "max_lum={}", md.max_luminance);
        assert!((md.min_luminance - 0.005).abs() < 1e-6, "min_lum={}", md.min_luminance);
        assert!((md.max_content_light_level - 1000.0).abs() < 1e-3);
        assert!((md.max_frame_average_light_level - 400.0).abs() < 1e-3);
    }

    /// Vulkan spec requires a non-empty surface format list. The
    /// picker debug-asserts this and a release build would index past
    /// the end of an empty slice — surface the requirement loudly so
    /// regressions in surface-cap querying don't get past tests.
    #[test]
    #[should_panic(expected = "spec requires at least one entry")]
    fn empty_surface_format_list_is_a_logic_bug() {
        let _ = pick_swapchain_format(&[], None);
    }
}
