// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Closed-form tone-curve reference implementations.
//!
//! Mirrors the GLSL implementations in
//! `vulkan/rhi/shaders/tone_curve.comp` — the GPU dispatch tests use
//! these CPU references to compute expected pixel values that the GPU
//! output is asserted against. Drift between the two is what those
//! tests catch.
//!
//! Two curves:
//! - **BT.2390 EETF** (`bt2390_eetf_per_channel`): forward HDR→SDR
//!   tone mapping. Closed-form Hermite spline in PQ space, per
//!   ITU-R Report BT.2390-9 Annex 2 §5.
//! - **BT.2446-1 method A2 inverse** (`bt2446a_inverse_per_channel`):
//!   SDR→HDR up-conversion. Closed-form Hermite ease in linear space.

use crate::core::color::{linear_to_pq, pq_to_linear};

/// Source-normalized linear → tone-mapped destination-normalized linear,
/// per channel, ITU-R BT.2390-9 EETF (Hermite spline in PQ space).
///
/// `linear_norm` is normalized so 1.0 = `peak_in_nits`. Output is
/// normalized so 1.0 = `peak_out_nits`.
///
/// The math is per channel: for canonical R/G/B tone mapping, call
/// once per channel with the same peak parameters.
///
/// At `linear_norm = 1.0` (peak input) the output is exactly 1.0 (peak
/// preserved). Below the knee point `KS = 1.5*maxLum - 0.5` (in the
/// PQ-normalized-to-source space) the curve is identity in PQ space;
/// above the knee a cubic Hermite spline interpolates from `(KS, KS)`
/// to `(1, maxLum)`.
pub fn bt2390_eetf_per_channel(linear_norm: f32, peak_in_nits: f32, peak_out_nits: f32) -> f32 {
    // Convert source-normalized linear → absolute linear (1.0 = 10000 nits).
    let linear_abs = linear_norm * peak_in_nits / 10_000.0;
    let pq_src = linear_to_pq(peak_in_nits / 10_000.0);
    let pq_dst = linear_to_pq(peak_out_nits / 10_000.0);
    let pq_in = linear_to_pq(linear_abs);

    // Normalize PQ to [0, 1] where 1 = source peak.
    let e1 = pq_in / pq_src;
    // Maximum allowed luminance in source-normalized PQ space.
    let max_lum = pq_dst / pq_src;
    // Knee start.
    let ks = 1.5 * max_lum - 0.5;

    let e2 = if e1 < ks {
        // Below knee: identity in PQ space.
        e1
    } else {
        // Hermite spline P(B) where B in [0, 1] from KS to 1.
        let denom = (1.0 - ks).max(1e-6);
        let t = (e1 - ks) / denom;
        let t2 = t * t;
        let t3 = t2 * t;
        (2.0 * t3 - 3.0 * t2 + 1.0) * ks
            + (t3 - 2.0 * t2 + t) * (1.0 - ks)
            + (-2.0 * t3 + 3.0 * t2) * max_lum
    };

    // Denormalize back to absolute PQ.
    let pq_out = e2 * pq_src;
    // Decode PQ → linear absolute.
    let lin_out_abs = pq_to_linear(pq_out);
    // Renormalize to destination peak.
    lin_out_abs * 10_000.0 / peak_out_nits
}

/// Source-normalized linear → expanded destination-normalized linear,
/// per channel, ITU-R BT.2446-1 method A2 inverse (closed-form gamma-
/// knee).
///
/// `linear_norm` is normalized so 1.0 = `peak_in_nits` (typically 100
/// for SDR sources). Output is normalized so 1.0 = `peak_out_nits`
/// (the HDR target, e.g., 1000 or 4000).
///
/// Below the knee at SDR=0.5 the curve is linear (preserves SDR
/// shadows + midtones at SDR-equivalent absolute nits); above the
/// knee a cubic Hermite ease expands into HDR headroom so that
/// `linear_norm = 1.0` maps to output `1.0` (peak preserved).
///
/// When `peak_out_nits <= peak_in_nits` the function passes through
/// (no expansion needed) — it normalizes by the peak ratio so that
/// the SDR content stays at the same absolute nits in the smaller
/// target range.
pub fn bt2446a_inverse_per_channel(linear_norm: f32, peak_in_nits: f32, peak_out_nits: f32) -> f32 {
    if peak_out_nits <= peak_in_nits {
        return linear_norm * peak_in_nits / peak_out_nits;
    }
    let ratio = peak_out_nits / peak_in_nits;
    const KNEE: f32 = 0.5;
    if linear_norm <= KNEE {
        return linear_norm / ratio;
    }
    let t = (linear_norm - KNEE) / (1.0 - KNEE);
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    let p0 = KNEE / ratio;
    let p1 = 1.0;
    let m0 = 1.0 / ratio;
    let m1 = 0.0;
    h00 * p0 + h10 * m0 + h01 * p1 + h11 * m1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Black input → black output: BT.2390 must map 0.0 → 0.0 exactly
    /// regardless of peak ratio. Mentally revert the
    /// `linear_norm * peak_in_nits / 10_000.0` line in `bt2390_eetf_per_channel`
    /// (e.g., to `linear_norm + peak_in_nits`) and this test fails
    /// because pq_in is no longer 0 at linear_norm=0.
    #[test]
    fn bt2390_zero_in_zero_out() {
        for &(pin, pout) in &[(1000.0, 100.0), (4000.0, 200.0), (10000.0, 1000.0)] {
            let out = bt2390_eetf_per_channel(0.0, pin, pout);
            assert!(out.abs() < 1e-5, "expected ~0, got {out}");
        }
    }

    /// Peak input → peak output: BT.2390 must map source peak (1.0
    /// normalized) to destination peak (1.0 normalized). This locks the
    /// "no peak loss" property the spline endpoint guarantees. Mentally
    /// revert the renormalization step (drop the `* 10_000.0 / peak_out_nits`)
    /// and this test fails — the output stays at peak_out/10000 instead
    /// of 1.0.
    #[test]
    fn bt2390_peak_input_maps_to_peak_output() {
        for &(pin, pout) in &[(1000.0, 100.0), (4000.0, 200.0), (10000.0, 1000.0)] {
            let out = bt2390_eetf_per_channel(1.0, pin, pout);
            assert!(
                (out - 1.0).abs() < 1e-3,
                "peak input must map to peak output: pin={pin} pout={pout} got {out}"
            );
        }
    }

    /// Identity case: when peak_in == peak_out, the EETF must be
    /// identity (input value passes through unchanged). The knee
    /// `ks = 1.5*1 - 0.5 = 1.0`, so the spline branch is never
    /// reached and the below-knee identity dominates everywhere.
    #[test]
    fn bt2390_identity_when_peaks_match() {
        for &x in &[0.0_f32, 0.1, 0.25, 0.5, 0.8, 1.0] {
            let out = bt2390_eetf_per_channel(x, 1000.0, 1000.0);
            assert!(
                (out - x).abs() < 1e-3,
                "identity must hold when peaks match: x={x} got {out}"
            );
        }
    }

    /// Monotonicity: a brighter input must produce an output at least
    /// as bright. Sample 65 points across [0, 1] and check pairwise.
    /// Mentally revert the spline `+ (-2.0 * t3 + 3.0 * t2) * max_lum`
    /// term (drop it) and this test fails because the spline becomes
    /// non-monotonic near the peak.
    #[test]
    fn bt2390_is_monotonic() {
        let pin = 1000.0;
        let pout = 100.0;
        let mut prev = bt2390_eetf_per_channel(0.0, pin, pout);
        for i in 1..=64 {
            let x = i as f32 / 64.0;
            let cur = bt2390_eetf_per_channel(x, pin, pout);
            assert!(
                cur + 1e-5 >= prev,
                "non-monotonic at x={x}: prev={prev} cur={cur}"
            );
            prev = cur;
        }
    }

    /// Print canonical 1000→100 nit reference points to stdout. Used
    /// once during development to populate the locked reference values
    /// in `bt2390_canonical_reference_points_1000_to_100`. Run with
    /// `cargo test bt2390_print -- --nocapture` to update if the
    /// implementation changes intentionally.
    #[test]
    #[ignore = "diagnostic-only — prints reference values for the lock test"]
    fn bt2390_print_reference_points_1000_to_100() {
        for input in [0.01_f32, 0.025, 0.05, 0.10, 0.25, 0.50, 0.75, 1.0] {
            let out = bt2390_eetf_per_channel(input, 1000.0, 100.0);
            println!("input={input:.4}  output={out:.6}");
        }
    }

    /// Mid-tone roll-off character: at HDR=1000 → SDR=100 (10× reduction)
    /// the BT.2390 curve should preserve shadows roughly linearly and
    /// roll off highlights into the smaller display range. Reference
    /// points captured from the implementation by
    /// [`bt2390_print_reference_points_1000_to_100`]; this test locks
    /// them so a future refactor that silently changes the curve shape
    /// fails. Tolerance is tight (1e-4) — the math is deterministic
    /// single-precision float, no platform variation expected.
    #[test]
    fn bt2390_canonical_reference_points_1000_to_100() {
        let pin = 1000.0;
        let pout = 100.0;
        // (input_norm, expected_output_norm) — input normalized to
        // 1000-nit peak; output normalized to 100-nit peak.
        let cases = [
            (0.01_f32, 0.099999_f32), // 10 nit input → 10 nit output (shadow, identity in PQ)
            (0.025_f32, 0.249999_f32), // 25 nit input → still identity (below knee)
            (0.05_f32, 0.461423_f32), // 50 nit input — knee onset
            (0.10_f32, 0.694542_f32), // 100 nit input — significant roll-off
            (0.25_f32, 0.920450_f32), // 250 nit input rolled toward peak
            (0.50_f32, 0.989461_f32), // 500 nit input close to peak
            (1.00_f32, 0.999997_f32), // peak preserved (within float tolerance)
        ];
        for (input, expected) in cases {
            let out = bt2390_eetf_per_channel(input, pin, pout);
            assert!(
                (out - expected).abs() < 1e-4,
                "BT.2390 1000→100: input={input} expected={expected} got {out} \
                 (delta {})",
                (out - expected).abs()
            );
        }
    }

    /// BT.2446a: black input → black output regardless of peak ratio.
    #[test]
    fn bt2446a_zero_in_zero_out() {
        for &(pin, pout) in &[(100.0, 1000.0), (100.0, 4000.0), (200.0, 1000.0)] {
            let out = bt2446a_inverse_per_channel(0.0, pin, pout);
            assert!(out.abs() < 1e-5, "expected ~0, got {out}");
        }
    }

    /// BT.2446a: peak input → peak output (1.0 SDR → 1.0 HDR
    /// normalized). Locks the cubic Hermite endpoint constraint.
    /// Mentally revert `p1 = 1.0` to `p1 = 0.5` and this test fails
    /// (output is 0.5 instead of 1.0 at peak).
    #[test]
    fn bt2446a_peak_input_maps_to_peak_output() {
        for &(pin, pout) in &[(100.0, 1000.0), (100.0, 4000.0), (200.0, 1000.0)] {
            let out = bt2446a_inverse_per_channel(1.0, pin, pout);
            assert!(
                (out - 1.0).abs() < 1e-5,
                "peak input must map to peak: pin={pin} pout={pout} got {out}"
            );
        }
    }

    /// BT.2446a: when peak_in == peak_out, the curve must be identity
    /// (no expansion needed; output equals input passed through the
    /// identity branch).
    #[test]
    fn bt2446a_identity_when_peaks_match() {
        for &x in &[0.0_f32, 0.1, 0.25, 0.5, 0.8, 1.0] {
            let out = bt2446a_inverse_per_channel(x, 100.0, 100.0);
            assert!(
                (out - x).abs() < 1e-5,
                "identity must hold when peaks match: x={x} got {out}"
            );
        }
    }

    /// BT.2446a: the linear shadow segment maps SDR midtone preserving
    /// absolute nits. SDR knee=0.5 in 100-nit space = 50 nit absolute.
    /// In HDR 1000-nit space, 50 nit absolute = 0.05 normalized.
    /// Mentally revert the `if linear_norm <= KNEE { return linear_norm / ratio; }`
    /// branch (drop the `/ ratio`) and this test fails with output 0.5
    /// instead of 0.05.
    #[test]
    fn bt2446a_below_knee_preserves_absolute_nits() {
        let pin = 100.0;
        let pout = 1000.0;
        let ratio = pout / pin;
        for &x in &[0.1_f32, 0.25, 0.49] {
            let expected = x / ratio;
            let out = bt2446a_inverse_per_channel(x, pin, pout);
            assert!(
                (out - expected).abs() < 1e-5,
                "below-knee must preserve absolute nits: x={x} expected={expected} got {out}"
            );
        }
    }

    /// BT.2446a: continuity at the knee — values just below and just
    /// above the knee point must produce nearly the same output (the
    /// Hermite ease is C0-continuous at t=0 by construction). Mentally
    /// revert `p0 = KNEE / ratio` to `p0 = KNEE` and this test fails
    /// because the cubic-segment endpoint no longer matches the
    /// linear-segment endpoint.
    #[test]
    fn bt2446a_continuous_at_knee() {
        let pin = 100.0;
        let pout = 1000.0;
        let just_below = bt2446a_inverse_per_channel(0.499, pin, pout);
        let just_above = bt2446a_inverse_per_channel(0.501, pin, pout);
        assert!(
            (just_above - just_below).abs() < 1e-3,
            "discontinuity at knee: below=0.499→{just_below} above=0.501→{just_above}"
        );
    }

    /// BT.2446a: monotonicity across the full input range.
    #[test]
    fn bt2446a_is_monotonic() {
        let pin = 100.0;
        let pout = 1000.0;
        let mut prev = bt2446a_inverse_per_channel(0.0, pin, pout);
        for i in 1..=64 {
            let x = i as f32 / 64.0;
            let cur = bt2446a_inverse_per_channel(x, pin, pout);
            assert!(
                cur + 1e-5 >= prev,
                "non-monotonic at x={x}: prev={prev} cur={cur}"
            );
            prev = cur;
        }
    }
}
