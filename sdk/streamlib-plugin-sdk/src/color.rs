// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-free color-math support types for the GPU surface.
//!
//! Pure-data ID enums + closed-form transfer functions + the YCbCr→RGB
//! matrix decomposition the color converter and the Vulkan-compute JPEG
//! kernel consume as push-constant state. These are byte-for-byte the
//! same shapes the engine carries in `core::color`; the SDK twin keeps
//! the plugin SDK engine-free (no `vulkanalia`, no engine) so a
//! Vulkan-compute-only plugin can build the same color push-constants
//! the host would.

// =============================================================================
// Transfer functions (transfer.rs twin)
// =============================================================================

/// Shader-side transfer-function identifier. The numeric value is what
/// gets passed through the push-constant `transfer_in` / `transfer_out`
/// slots — the shader switches on it.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransferId {
    /// No transfer applied (pass-through).
    Linear = 0,
    /// sRGB / IEC 61966-2-1.
    Srgb = 1,
    /// Rec.709 OETF (also Rec.601, BT.2020 SDR).
    Bt709 = 2,
    /// SMPTE 2084 PQ (HDR10). Reference: 10000 nit display.
    Pq = 3,
    /// ARIB STD-B67 / HLG. Reference: 1000 nit display.
    Hlg = 4,
}

/// sRGB EOTF: encoded → linear.
pub fn srgb_to_linear(x: f32) -> f32 {
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB OETF: linear → encoded.
pub fn linear_to_srgb(x: f32) -> f32 {
    if x <= 0.0031308 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// Rec.709 OETF inverse: encoded → linear.
pub fn bt709_to_linear(x: f32) -> f32 {
    if x < 0.081 {
        x / 4.5
    } else {
        ((x + 0.099) / 1.099).powf(1.0 / 0.45)
    }
}

/// Rec.709 OETF: linear → encoded.
pub fn linear_to_bt709(x: f32) -> f32 {
    if x < 0.018 {
        4.5 * x
    } else {
        1.099 * x.powf(0.45) - 0.099
    }
}

const PQ_M1: f32 = 2610.0 / 16384.0;
const PQ_M2: f32 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f32 = 2392.0 / 4096.0 * 32.0;

/// SMPTE 2084 PQ EOTF: encoded `[0,1]` → linear (referenced to 10000 nit).
pub fn pq_to_linear(x: f32) -> f32 {
    let xp = x.max(0.0).powf(1.0 / PQ_M2);
    let num = (xp - PQ_C1).max(0.0);
    let den = PQ_C2 - PQ_C3 * xp;
    (num / den).powf(1.0 / PQ_M1)
}

/// SMPTE 2084 PQ OETF: linear (referenced to 10000 nit) → encoded `[0,1]`.
pub fn linear_to_pq(x: f32) -> f32 {
    let xp = x.max(0.0).powf(PQ_M1);
    let num = PQ_C1 + PQ_C2 * xp;
    let den = 1.0 + PQ_C3 * xp;
    (num / den).powf(PQ_M2)
}

const HLG_A: f32 = 0.17883277;
const HLG_B: f32 = 0.28466892;
const HLG_C: f32 = 0.55991073;

/// ARIB STD-B67 HLG OETF inverse: encoded `[0,1]` → linear `[0,1]`.
pub fn hlg_to_linear(x: f32) -> f32 {
    if x <= 0.5 {
        (x * x) / 3.0
    } else {
        (((x - HLG_C) / HLG_A).exp() + HLG_B) / 12.0
    }
}

/// ARIB STD-B67 HLG OETF: linear `[0,1]` → encoded `[0,1]`.
pub fn linear_to_hlg(x: f32) -> f32 {
    if x <= 1.0 / 12.0 {
        (3.0 * x.max(0.0)).sqrt()
    } else {
        HLG_A * (12.0 * x - HLG_B).ln() + HLG_C
    }
}

/// CPU reference for the shader's transfer switch.
pub fn to_linear(id: TransferId, x: f32) -> f32 {
    match id {
        TransferId::Linear => x,
        TransferId::Srgb => srgb_to_linear(x),
        TransferId::Bt709 => bt709_to_linear(x),
        TransferId::Pq => pq_to_linear(x),
        TransferId::Hlg => hlg_to_linear(x),
    }
}

/// CPU reference for the shader's inverse-transfer switch.
pub fn from_linear(id: TransferId, x: f32) -> f32 {
    match id {
        TransferId::Linear => x,
        TransferId::Srgb => linear_to_srgb(x),
        TransferId::Bt709 => linear_to_bt709(x),
        TransferId::Pq => linear_to_pq(x),
        TransferId::Hlg => linear_to_hlg(x),
    }
}

// =============================================================================
// Resolved color description (resolved.rs twin)
// =============================================================================

/// Color-primaries id. Mirrors H.273 `ColourPrimaries` variants.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimariesId {
    Bt709,
    Bt470M,
    Bt470Bg,
    Smpte170m,
    Smpte240m,
    Film,
    Bt2020,
    Smpte428,
    Smpte431,
    Smpte432,
    Ebu3213,
}

/// YCbCr-matrix id. Mirrors H.273 `MatrixCoefficients` variants.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatrixId {
    Identity,
    Bt709,
    Fcc,
    Bt470Bg,
    Smpte170m,
    Smpte240m,
    Ycgco,
    Bt2020Ncl,
    Bt2020Cl,
    Smpte2085,
    ChromaNcl,
    ChromaCl,
    Ictcp,
}

/// Quantization range id. Maps to H.264/H.265 VUI `video_full_range_flag`
/// (`Limited` = 0, `Full` = 1).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RangeId {
    Limited,
    Full,
}

/// Color description with every axis resolved to a concrete value. The
/// converter consumes this; the on-wire `ColorInfo` is a sparse
/// `Option<T>`-per-axis projection of the same shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResolvedColorInfo {
    pub primaries: PrimariesId,
    pub transfer: TransferId,
    pub matrix: MatrixId,
    pub range: RangeId,
}

/// Disambiguator for per-axis defaults. RGB-encoded sources default
/// transfer→`Srgb` and range→`Full`; YCbCr-encoded sources default
/// transfer→`Bt709` and range→`Limited`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpaceKind {
    /// RGB / BGRA / packed-RGB source (matrix axis collapses to
    /// `Identity` regardless of the on-wire matrix enum).
    Rgb,
    /// YCbCr / NV12 / YUYV source (matrix axis honors the on-wire value).
    Yuv,
}

/// Resolve each absent axis (`None` = the on-wire value was H.273 "Unspecified")
/// to its per-kind default, mirroring V4L2's `V4L2_MAP_*_DEFAULT` and libplacebo's
/// `pl_color_space_infer`. RGB sources collapse the matrix axis to `Identity`.
pub fn resolve_color_defaults(
    primaries: Option<PrimariesId>,
    transfer: Option<TransferId>,
    matrix: Option<MatrixId>,
    range: Option<RangeId>,
    kind: ColorSpaceKind,
) -> ResolvedColorInfo {
    let primaries = primaries.unwrap_or(PrimariesId::Bt709);
    let transfer = transfer.unwrap_or(match kind {
        ColorSpaceKind::Rgb => TransferId::Srgb,
        ColorSpaceKind::Yuv => TransferId::Bt709,
    });
    let matrix = match kind {
        ColorSpaceKind::Rgb => MatrixId::Identity,
        ColorSpaceKind::Yuv => matrix.unwrap_or(MatrixId::Smpte170m),
    };
    let range = range.unwrap_or(match kind {
        ColorSpaceKind::Rgb => RangeId::Full,
        ColorSpaceKind::Yuv => RangeId::Limited,
    });
    ResolvedColorInfo {
        primaries,
        transfer,
        matrix,
        range,
    }
}

// =============================================================================
// YCbCr → RGB matrix decomposition (matrix.rs twin)
// =============================================================================

/// Output of [`yuv_to_rgb_matrix`]. Row-major 3×3 matrix plus a
/// per-channel offset that is subtracted from byte-domain YCbCr before
/// the matrix is applied.
pub struct YuvToRgbDecomposition {
    /// Row-major: `[r·y, r·cb, r·cr, g·y, g·cb, g·cr, b·y, b·cb, b·cr]`.
    pub matrix_row_major: [f32; 9],
    /// `(y_offset, cb_offset, cr_offset)` in 8-bit byte units.
    pub offset: [f32; 3],
}

/// Decompose `(matrix, range)` into a 3×3 YCbCr→RGB matrix plus byte-
/// domain offset. The matrix already incorporates range-expansion scale.
pub fn yuv_to_rgb_matrix(matrix: MatrixId, range: RangeId) -> YuvToRgbDecomposition {
    if matches!(matrix, MatrixId::Identity) {
        return YuvToRgbDecomposition {
            matrix_row_major: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            offset: [0.0, 0.0, 0.0],
        };
    }

    let (kr, kb) = kr_kb(matrix);
    let kg = 1.0 - kr - kb;
    let (y_scale, c_scale, y_offset) = range_scaling(range);

    let m_r_cr = 2.0 * (1.0 - kr) * c_scale;
    let m_g_cb = -2.0 * (1.0 - kb) * kb / kg * c_scale;
    let m_g_cr = -2.0 * (1.0 - kr) * kr / kg * c_scale;
    let m_b_cb = 2.0 * (1.0 - kb) * c_scale;

    YuvToRgbDecomposition {
        matrix_row_major: [
            y_scale, 0.0, m_r_cr, y_scale, m_g_cb, m_g_cr, y_scale, m_b_cb, 0.0,
        ],
        offset: [y_offset, 128.0, 128.0],
    }
}

/// `(Kr, Kb)` for each H.273 matrix enumerant. Unmapped variants fall
/// back to BT.601 525-line.
fn kr_kb(matrix: MatrixId) -> (f32, f32) {
    match matrix {
        MatrixId::Bt709 => (0.2126, 0.0722),
        MatrixId::Smpte170m | MatrixId::Bt470Bg => (0.299, 0.114),
        MatrixId::Fcc => (0.30, 0.11),
        MatrixId::Smpte240m => (0.212, 0.087),
        MatrixId::Bt2020Ncl | MatrixId::Bt2020Cl => (0.2627, 0.0593),
        _ => (0.299, 0.114),
    }
}

/// Returns `(y_scale, c_scale, y_offset)` in byte-domain units.
fn range_scaling(range: RangeId) -> (f32, f32, f32) {
    match range {
        RangeId::Limited => (255.0 / 219.0, 255.0 / 224.0, 16.0),
        RangeId::Full => (1.0, 1.0, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_round_trip_midgray() {
        let lin = srgb_to_linear(0.5);
        assert!((lin - 0.2140).abs() < 1e-3, "expected ~0.2140, got {lin}");
        assert!((linear_to_srgb(lin) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn bt601_limited_range_matches_canonical_coefficients() {
        let d = yuv_to_rgb_matrix(MatrixId::Smpte170m, RangeId::Limited);
        assert!((d.matrix_row_major[0] - 1.164).abs() < 5e-3);
        assert_eq!(d.offset, [16.0, 128.0, 128.0]);
    }

    #[test]
    fn identity_returns_identity_matrix() {
        let d = yuv_to_rgb_matrix(MatrixId::Identity, RangeId::Full);
        assert_eq!(
            d.matrix_row_major,
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
        );
        assert_eq!(d.offset, [0.0, 0.0, 0.0]);
    }

    /// All-None inputs for an RGB source resolve to
    /// `Bt709 / Srgb / Identity / Full`.
    #[test]
    fn rgb_all_none_defaults_match_table() {
        let r = resolve_color_defaults(None, None, None, None, ColorSpaceKind::Rgb);
        assert_eq!(r.primaries, PrimariesId::Bt709);
        assert_eq!(r.transfer, TransferId::Srgb);
        assert_eq!(r.matrix, MatrixId::Identity);
        assert_eq!(r.range, RangeId::Full);
    }

    /// All-None inputs for a YCbCr source resolve to the UVC
    /// convention: `Bt709 / Bt709 / Smpte170m / Limited`.
    #[test]
    fn yuv_all_none_defaults_match_table() {
        let r = resolve_color_defaults(None, None, None, None, ColorSpaceKind::Yuv);
        assert_eq!(r.primaries, PrimariesId::Bt709);
        assert_eq!(r.transfer, TransferId::Bt709);
        assert_eq!(r.matrix, MatrixId::Smpte170m);
        assert_eq!(r.range, RangeId::Limited);
    }

    /// Explicit values on every axis pass through unchanged for YUV.
    #[test]
    fn explicit_values_pass_through_yuv() {
        let r = resolve_color_defaults(
            Some(PrimariesId::Bt2020),
            Some(TransferId::Pq),
            Some(MatrixId::Bt2020Ncl),
            Some(RangeId::Limited),
            ColorSpaceKind::Yuv,
        );
        assert_eq!(r.primaries, PrimariesId::Bt2020);
        assert_eq!(r.transfer, TransferId::Pq);
        assert_eq!(r.matrix, MatrixId::Bt2020Ncl);
        assert_eq!(r.range, RangeId::Limited);
    }

    /// RGB sources override the matrix axis to `Identity` even when
    /// the input claims otherwise. Matrix coefficients are meaningless
    /// for already-RGB data.
    #[test]
    fn rgb_overrides_matrix_to_identity() {
        let r = resolve_color_defaults(
            Some(PrimariesId::Bt709),
            Some(TransferId::Srgb),
            Some(MatrixId::Bt709),
            Some(RangeId::Full),
            ColorSpaceKind::Rgb,
        );
        assert_eq!(r.matrix, MatrixId::Identity);
    }

    /// Partial info: only matrix specified. Other axes pull defaults.
    #[test]
    fn partial_info_fills_only_missing_axes() {
        let r =
            resolve_color_defaults(None, None, Some(MatrixId::Bt709), None, ColorSpaceKind::Yuv);
        assert_eq!(r.matrix, MatrixId::Bt709); // explicit
        assert_eq!(r.primaries, PrimariesId::Bt709); // default
        assert_eq!(r.transfer, TransferId::Bt709); // YUV default
        assert_eq!(r.range, RangeId::Limited); // YUV default
    }
}
