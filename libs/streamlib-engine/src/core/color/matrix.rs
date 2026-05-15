// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! YCbCr → RGB matrix decomposition for closed-form conversion.
//!
//! The shader applies `rgb_byte = M * (ycbcr_byte - offset)` and then
//! `rgb_normalized = clamp(rgb_byte / 255, 0, 1)`. `M` and `offset`
//! are pushed per-frame via push constants; they're derived from the
//! `(matrix, range)` pair of [`ResolvedColorInfo`].
//!
//! The matrix returned here bakes the range-expansion scale into the
//! 3×3 — i.e. for BT.601 limited the first column is `1.164` (which is
//! `255/219`), and the chroma columns include the `255/224` factor.
//! This collapses range expansion + YCbCr→RGB into a single matrix
//! multiply on the GPU.

use crate::_generated_::tatolab__core::color_info::{Matrix, Range};

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
pub fn yuv_to_rgb_matrix(matrix: Matrix, range: Range) -> YuvToRgbDecomposition {
    if matches!(matrix, Matrix::Identity) {
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
            y_scale, 0.0, m_r_cr,
            y_scale, m_g_cb, m_g_cr,
            y_scale, m_b_cb, 0.0,
        ],
        offset: [y_offset, 128.0, 128.0],
    }
}

/// `(Kr, Kb)` for each H.273 matrix enumerant. Unmapped variants fall
/// back to BT.601 525-line.
fn kr_kb(matrix: Matrix) -> (f32, f32) {
    match matrix {
        Matrix::Bt709 => (0.2126, 0.0722),
        Matrix::Smpte170m | Matrix::Bt470Bg => (0.299, 0.114),
        Matrix::Fcc => (0.30, 0.11),
        Matrix::Smpte240m => (0.212, 0.087),
        Matrix::Bt2020Ncl | Matrix::Bt2020Cl => (0.2627, 0.0593),
        // YCgCo / ICtCp / Smpte2085 / ChromaNcl / ChromaCl have
        // distinct math the linear-matrix decomposition does not
        // cover. Falling back to BT.601 is a coarse approximation —
        // a future pass routes these through dedicated paths.
        _ => (0.299, 0.114),
    }
}

/// Returns `(y_scale, c_scale, y_offset)` in byte-domain units.
fn range_scaling(range: Range) -> (f32, f32, f32) {
    match range {
        Range::Limited => (255.0 / 219.0, 255.0 / 224.0, 16.0),
        Range::Full => (1.0, 1.0, 0.0),
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
        let d = yuv_to_rgb_matrix(Matrix::Smpte170m, Range::Full);
        // Y_scale=1, c_scale=1, y_offset=0, BT.601 coefficients
        let expected = [
            1.0, 0.0, 1.402,
            1.0, -0.344136, -0.714136,
            1.0, 1.772, 0.0,
        ];
        assert_matrix(&d.matrix_row_major, &expected, 1e-4);
        assert_eq!(d.offset, [0.0, 128.0, 128.0]);
    }

    /// BT.601 limited-range — the classic camera/decoder matrix with
    /// 1.164 Y_scale.
    #[test]
    fn bt601_limited_range_matches_canonical_coefficients() {
        let d = yuv_to_rgb_matrix(Matrix::Smpte170m, Range::Limited);
        // The widely-quoted limited matrix:
        // R = 1.164 Y' + 1.596 Cr'
        // G = 1.164 Y' - 0.392 Cb' - 0.813 Cr'
        // B = 1.164 Y' + 2.017 Cb'
        let expected = [
            1.164, 0.0, 1.596,
            1.164, -0.392, -0.813,
            1.164, 2.017, 0.0,
        ];
        assert_matrix(&d.matrix_row_major, &expected, 5e-3);
        assert_eq!(d.offset, [16.0, 128.0, 128.0]);
    }

    /// BT.709 full-range.
    #[test]
    fn bt709_full_range_matches_canonical_coefficients() {
        let d = yuv_to_rgb_matrix(Matrix::Bt709, Range::Full);
        let expected = [
            1.0, 0.0, 1.5748,
            1.0, -0.1873, -0.4681,
            1.0, 1.8556, 0.0,
        ];
        assert_matrix(&d.matrix_row_major, &expected, 5e-4);
        assert_eq!(d.offset, [0.0, 128.0, 128.0]);
    }

    /// BT.709 limited-range — modern camera + h264/h265 codec default.
    #[test]
    fn bt709_limited_range_matches_canonical_coefficients() {
        let d = yuv_to_rgb_matrix(Matrix::Bt709, Range::Limited);
        // 1.164 = 255/219; 1.793 = 1.5748 * 255/224; etc.
        let expected = [
            1.164, 0.0, 1.793,
            1.164, -0.213, -0.533,
            1.164, 2.112, 0.0,
        ];
        assert_matrix(&d.matrix_row_major, &expected, 5e-3);
        assert_eq!(d.offset, [16.0, 128.0, 128.0]);
    }

    /// BT.2020 NCL limited — HDR pipeline staple.
    #[test]
    fn bt2020_ncl_limited_range_matches_canonical_coefficients() {
        let d = yuv_to_rgb_matrix(Matrix::Bt2020Ncl, Range::Limited);
        // Kr=0.2627, Kb=0.0593, Kg=0.6780
        // R: 1.164, 0, 1.4746*255/224 ≈ 1.679
        // G: 1.164, -(2*0.9407*0.0593/0.678)*255/224 ≈ -0.187,
        //          -(2*0.7373*0.2627/0.678)*255/224 ≈ -0.650
        // B: 1.164, (2*0.9407)*255/224 ≈ 2.142, 0
        let expected = [
            1.164, 0.0, 1.679,
            1.164, -0.187, -0.650,
            1.164, 2.142, 0.0,
        ];
        assert_matrix(&d.matrix_row_major, &expected, 5e-3);
        assert_eq!(d.offset, [16.0, 128.0, 128.0]);
    }

    /// Identity matrix → 3×3 identity, zero offset. Pass-through for
    /// RGB-encoded sources.
    #[test]
    fn identity_returns_identity_matrix() {
        let d = yuv_to_rgb_matrix(Matrix::Identity, Range::Full);
        let expected = [
            1.0, 0.0, 0.0,
            0.0, 1.0, 0.0,
            0.0, 0.0, 1.0,
        ];
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
        let limited = yuv_to_rgb_matrix(Matrix::Smpte170m, Range::Limited);
        let full = yuv_to_rgb_matrix(Matrix::Smpte170m, Range::Full);
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
}
