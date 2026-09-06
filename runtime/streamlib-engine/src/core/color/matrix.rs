// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! YCbCr ↔ RGB matrix decompositions for closed-form conversion.
//!
//! The decoding shaders apply `rgb_byte = M * (ycbcr_byte - offset)` and
//! then `rgb_normalized = clamp(rgb_byte / 255, 0, 1)`; the encoding
//! shader applies `ycbcr_byte = M' * rgb_byte + offset`. `M` and `offset`
//! are pushed per-frame via push constants; they're derived from the
//! `(matrix, range)` pair of [`ResolvedColorInfo`].
//!
//! The matrix returned here bakes the range-expansion scale into the
//! 3×3 — i.e. for BT.601 limited the first column is `1.164` (which is
//! `255/219`), and the chroma columns include the `255/224` factor.
//! This collapses range expansion + YCbCr→RGB into a single matrix
//! multiply on the GPU.

use super::{MatrixId, RangeId};

/// Output of [`yuv_to_rgb_matrix`]. Row-major 3×3 matrix plus a
/// per-channel offset that is subtracted from byte-domain YCbCr before
/// the matrix is applied.
pub struct YuvToRgbDecomposition {
    /// Row-major: `[r·y, r·cb, r·cr, g·y, g·cb, g·cr, b·y, b·cb, b·cr]`.
    pub matrix_row_major: [f32; 9],
    /// `(y_offset, cb_offset, cr_offset)` in 8-bit byte units. The shader
    /// subtracts this from the raw byte-domain YCbCr triple before the
    /// matrix multiply.
    pub offset: [f32; 3],
}

/// Decompose `(matrix, range)` into a 3×3 YCbCr→RGB matrix plus byte-
/// domain offset. The matrix already incorporates range-expansion
/// scale.
///
/// `matrix = Identity` returns the identity matrix with zero offset —
/// pass-through for RGB-encoded sources (no YCbCr conversion needed).
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
        // YCgCo / ICtCp / Smpte2085 / ChromaNcl / ChromaCl have
        // distinct math the linear-matrix decomposition does not
        // cover. Falling back to BT.601 is a coarse approximation —
        // a future pass routes these through dedicated paths.
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

/// Output of [`rgb_to_yuv_matrix`] — the inverse of
/// [`YuvToRgbDecomposition`] off the same `(Kr, Kb)` and range table, so
/// a byte round-trip through both is the identity to rounding.
pub struct RgbToYuvDecomposition {
    /// Row-major: `[y·r, y·g, y·b, cb·r, cb·g, cb·b, cr·r, cr·g, cr·b]`.
    pub matrix_row_major: [f32; 9],
    /// `(y_offset, cb_offset, cr_offset)` in 8-bit byte units, added to
    /// the byte-domain product.
    pub offset: [f32; 3],
}

/// Decompose `(matrix, range)` into a 3×3 RGB→YCbCr matrix plus byte-
/// domain offset. The matrix already incorporates range compression.
///
/// `matrix = Identity` returns the identity matrix with zero offset.
pub fn rgb_to_yuv_matrix(matrix: MatrixId, range: RangeId) -> RgbToYuvDecomposition {
    if matches!(matrix, MatrixId::Identity) {
        return RgbToYuvDecomposition {
            matrix_row_major: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            offset: [0.0, 0.0, 0.0],
        };
    }

    let (kr, kb) = kr_kb(matrix);
    let kg = 1.0 - kr - kb;
    let (y_scale, c_scale, y_offset) = range_scaling(range);
    let y_compression = 1.0 / y_scale;
    let c_compression = 1.0 / c_scale;

    // Cb = (B − Y′) / (2(1 − Kb)) and Cr = (R − Y′) / (2(1 − Kr)), with
    // Y′ = Kr·R + Kg·G + Kb·B substituted in.
    let cb_denominator = 2.0 * (1.0 - kb);
    let cr_denominator = 2.0 * (1.0 - kr);

    RgbToYuvDecomposition {
        matrix_row_major: [
            kr * y_compression,
            kg * y_compression,
            kb * y_compression,
            -kr / cb_denominator * c_compression,
            -kg / cb_denominator * c_compression,
            0.5 * c_compression,
            0.5 * c_compression,
            -kg / cr_denominator * c_compression,
            -kb / cr_denominator * c_compression,
        ],
        offset: [y_offset, 128.0, 128.0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    fn assert_matrix(m: &[f32; 9], expected: &[f32; 9], eps: f32) {
        for (i, (a, e)) in m.iter().zip(expected.iter()).enumerate() {
            assert!(
                approx_eq(*a, *e, eps),
                "row-major[{i}] mismatch: got {a}, expected {e}"
            );
        }
    }

    /// BT.601 full-range — classic webcam / JPEG matrix.
    #[test]
    fn bt601_full_range_matches_canonical_coefficients() {
        let d = yuv_to_rgb_matrix(MatrixId::Smpte170m, RangeId::Full);
        // Y_scale=1, c_scale=1, y_offset=0, BT.601 coefficients
        let expected = [1.0, 0.0, 1.402, 1.0, -0.344136, -0.714136, 1.0, 1.772, 0.0];
        assert_matrix(&d.matrix_row_major, &expected, 1e-4);
        assert_eq!(d.offset, [0.0, 128.0, 128.0]);
    }

    /// BT.601 limited-range — the classic camera/decoder matrix with
    /// 1.164 Y_scale.
    #[test]
    fn bt601_limited_range_matches_canonical_coefficients() {
        let d = yuv_to_rgb_matrix(MatrixId::Smpte170m, RangeId::Limited);
        // The widely-quoted limited matrix:
        // R = 1.164 Y' + 1.596 Cr'
        // G = 1.164 Y' - 0.392 Cb' - 0.813 Cr'
        // B = 1.164 Y' + 2.017 Cb'
        let expected = [1.164, 0.0, 1.596, 1.164, -0.392, -0.813, 1.164, 2.017, 0.0];
        assert_matrix(&d.matrix_row_major, &expected, 5e-3);
        assert_eq!(d.offset, [16.0, 128.0, 128.0]);
    }

    /// BT.709 full-range.
    #[test]
    fn bt709_full_range_matches_canonical_coefficients() {
        let d = yuv_to_rgb_matrix(MatrixId::Bt709, RangeId::Full);
        let expected = [1.0, 0.0, 1.5748, 1.0, -0.1873, -0.4681, 1.0, 1.8556, 0.0];
        assert_matrix(&d.matrix_row_major, &expected, 5e-4);
        assert_eq!(d.offset, [0.0, 128.0, 128.0]);
    }

    /// BT.709 limited-range — modern camera + h264/h265 codec default.
    #[test]
    fn bt709_limited_range_matches_canonical_coefficients() {
        let d = yuv_to_rgb_matrix(MatrixId::Bt709, RangeId::Limited);
        // 1.164 = 255/219; 1.793 = 1.5748 * 255/224; etc.
        let expected = [1.164, 0.0, 1.793, 1.164, -0.213, -0.533, 1.164, 2.112, 0.0];
        assert_matrix(&d.matrix_row_major, &expected, 5e-3);
        assert_eq!(d.offset, [16.0, 128.0, 128.0]);
    }

    /// BT.2020 NCL limited — HDR pipeline staple.
    #[test]
    fn bt2020_ncl_limited_range_matches_canonical_coefficients() {
        let d = yuv_to_rgb_matrix(MatrixId::Bt2020Ncl, RangeId::Limited);
        // Kr=0.2627, Kb=0.0593, Kg=0.6780
        // R: 1.164, 0, 1.4746*255/224 ≈ 1.679
        // G: 1.164, -(2*0.9407*0.0593/0.678)*255/224 ≈ -0.187,
        //          -(2*0.7373*0.2627/0.678)*255/224 ≈ -0.650
        // B: 1.164, (2*0.9407)*255/224 ≈ 2.142, 0
        let expected = [1.164, 0.0, 1.679, 1.164, -0.187, -0.650, 1.164, 2.142, 0.0];
        assert_matrix(&d.matrix_row_major, &expected, 5e-3);
        assert_eq!(d.offset, [16.0, 128.0, 128.0]);
    }

    /// Identity matrix → 3×3 identity, zero offset. Pass-through for
    /// RGB-encoded sources.
    #[test]
    fn identity_returns_identity_matrix() {
        let d = yuv_to_rgb_matrix(MatrixId::Identity, RangeId::Full);
        let expected = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        assert_matrix(&d.matrix_row_major, &expected, 0.0);
        assert_eq!(d.offset, [0.0, 0.0, 0.0]);
    }

    /// Reverting the range-expansion factor in `range_scaling` (e.g.
    /// returning `(1.0, 1.0, 0.0)` for `Limited`) drops the `1.164`
    /// from the BT.601 limited matrix. This test guards against that
    /// regression — mentally revert the `255/219` to `1.0` and the
    /// expected matrix below stops matching.
    #[test]
    fn limited_range_actually_scales_y() {
        let limited = yuv_to_rgb_matrix(MatrixId::Smpte170m, RangeId::Limited);
        let full = yuv_to_rgb_matrix(MatrixId::Smpte170m, RangeId::Full);
        assert!(
            (limited.matrix_row_major[0] - full.matrix_row_major[0]).abs() > 0.1,
            "limited-range Y scale must differ from full-range Y scale; \
             got limited={}, full={}",
            limited.matrix_row_major[0],
            full.matrix_row_major[0]
        );
        // Limited Y offset must be 16, full must be 0.
        assert_eq!(limited.offset[0], 16.0);
        assert_eq!(full.offset[0], 0.0);
    }

    fn apply_rgb_to_yuv(d: &RgbToYuvDecomposition, rgb: [f32; 3]) -> [f32; 3] {
        let m = &d.matrix_row_major;
        [
            m[0] * rgb[0] + m[1] * rgb[1] + m[2] * rgb[2] + d.offset[0],
            m[3] * rgb[0] + m[4] * rgb[1] + m[5] * rgb[2] + d.offset[1],
            m[6] * rgb[0] + m[7] * rgb[1] + m[8] * rgb[2] + d.offset[2],
        ]
    }

    fn apply_yuv_to_rgb(d: &YuvToRgbDecomposition, ycbcr: [f32; 3]) -> [f32; 3] {
        let m = &d.matrix_row_major;
        let c = [
            ycbcr[0] - d.offset[0],
            ycbcr[1] - d.offset[1],
            ycbcr[2] - d.offset[2],
        ];
        [
            m[0] * c[0] + m[1] * c[1] + m[2] * c[2],
            m[3] * c[0] + m[4] * c[1] + m[5] * c[2],
            m[6] * c[0] + m[7] * c[1] + m[8] * c[2],
        ]
    }

    /// The encoding matrix is the inverse of the decoding one: a byte
    /// triple survives RGB → YCbCr → RGB through both tables to well under
    /// a quantisation step, for every matrix and range the engine names.
    #[test]
    fn rgb_to_yuv_is_the_inverse_of_yuv_to_rgb_for_every_matrix_and_range() {
        let matrices = [
            MatrixId::Bt709,
            MatrixId::Smpte170m,
            MatrixId::Bt470Bg,
            MatrixId::Fcc,
            MatrixId::Smpte240m,
            MatrixId::Bt2020Ncl,
        ];
        let samples = [
            [0.0, 0.0, 0.0],
            [255.0, 255.0, 255.0],
            [200.0, 100.0, 50.0],
            [12.0, 240.0, 133.0],
            [255.0, 0.0, 0.0],
            [0.0, 0.0, 255.0],
        ];
        for matrix in matrices {
            for range in [RangeId::Limited, RangeId::Full] {
                let forward = rgb_to_yuv_matrix(matrix, range);
                let back = yuv_to_rgb_matrix(matrix, range);
                for rgb in samples {
                    let round_tripped = apply_yuv_to_rgb(&back, apply_rgb_to_yuv(&forward, rgb));
                    for channel in 0..3 {
                        assert!(
                            approx_eq(round_tripped[channel], rgb[channel], 1e-2),
                            "{matrix:?}/{range:?}: {rgb:?} came back as {round_tripped:?}"
                        );
                    }
                }
            }
        }
    }

    /// BT.601 limited: reference white lands on the range ceiling and
    /// black on its floor, with neutral chroma at both.
    #[test]
    fn bt601_limited_maps_white_to_235_and_black_to_16_with_neutral_chroma() {
        let d = rgb_to_yuv_matrix(MatrixId::Smpte170m, RangeId::Limited);
        let white = apply_rgb_to_yuv(&d, [255.0, 255.0, 255.0]);
        let black = apply_rgb_to_yuv(&d, [0.0, 0.0, 0.0]);
        assert!(approx_eq(white[0], 235.0, 1e-2), "white Y = {}", white[0]);
        assert!(approx_eq(black[0], 16.0, 1e-3), "black Y = {}", black[0]);
        for value in [white[1], white[2], black[1], black[2]] {
            assert!(approx_eq(value, 128.0, 1e-2), "neutral chroma = {value}");
        }
    }

    /// The identity matrix encodes RGB as RGB: no offset, no compression.
    #[test]
    fn identity_rgb_to_yuv_is_a_pass_through() {
        let d = rgb_to_yuv_matrix(MatrixId::Identity, RangeId::Limited);
        assert_eq!(
            d.matrix_row_major,
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
        );
        assert_eq!(d.offset, [0.0, 0.0, 0.0]);
    }
}
