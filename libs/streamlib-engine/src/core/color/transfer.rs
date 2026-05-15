// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Closed-form transfer functions (EOTF / OETF inverse).
//!
//! Mirrors the closed-form implementations used by libplacebo, GStreamer
//! `glcolorconvert`, and Chromium `ui/gfx/color_transform`. Each function
//! converts a single channel value; channels are processed independently.
//!
//! Two directions per curve:
//! - **EOTF / `to_linear`**: encoded value → scene-linear normalized
//!   intensity. Input in the canonical encoded range for that curve;
//!   output approximately `[0, 1]` for SDR or `[0, ~10000/100]` for PQ
//!   (PQ is referenced to 10000 nit displays).
//! - **OETF inverse / `from_linear`**: scene-linear → encoded. Inverse
//!   of the above; round-trip is within `2^-16` per channel.
//!
//! These CPU implementations exist for unit-test reference. The same
//! math is inlined in the converter shader (see
//! `vulkan/rhi/shaders/color_convert_common.glsl`) — keeping the two
//! synchronized is what the `transfer_round_trip_*` tests guard.

/// Shader-side transfer-function identifier. The numeric value is what
/// gets passed through the push-constant `transfer_in` / `transfer_out`
/// slots — the shader switches on it. Must stay in sync with the
/// `TRANSFER_*` constants in the shader.
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

impl TransferId {
    /// Map a (best-effort resolved) `Transfer` enum to a shader id.
    /// Unmapped curves fall back to `Linear` — they're rare in
    /// practice and the converter applies no transfer in that case.
    pub fn from_transfer(t: crate::_generated_::tatolab__core::color_info::Transfer) -> Self {
        use crate::_generated_::tatolab__core::color_info::Transfer;
        match t {
            Transfer::Srgb => TransferId::Srgb,
            Transfer::Bt709
            | Transfer::Smpte170m
            | Transfer::Bt2020TenBit
            | Transfer::Bt2020TwelveBit => TransferId::Bt709,
            Transfer::Smpte2084 => TransferId::Pq,
            Transfer::AribStdB67 => TransferId::Hlg,
            Transfer::Linear => TransferId::Linear,
            // Gamma22 / Gamma28 / Smpte240m / Log* / Xvycc / Bt1361 / Smpte428
            // are uncommon end-to-end; map to Linear (no transform) for now.
            // A future pass can extend the shader's switch.
            _ => TransferId::Linear,
        }
    }
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

/// Rec.709 OETF inverse: encoded → linear. Also used for BT.601
/// and BT.2020 SDR (same curve).
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
/// Linear `1.0` corresponds to the reference peak (12× the diffuse-gray
/// linear scene level).
pub fn hlg_to_linear(x: f32) -> f32 {
    if x <= 0.5 {
        (x * x) / 3.0
    } else {
        (((x - HLG_C) / HLG_A).exp() + HLG_B) / 12.0
    }
}

/// ARIB STD-B67 HLG OETF: linear `[0,1]` → encoded `[0,1]`. Linear `1.0`
/// is the reference peak (12× diffuse gray).
pub fn linear_to_hlg(x: f32) -> f32 {
    if x <= 1.0 / 12.0 {
        (3.0 * x.max(0.0)).sqrt()
    } else {
        HLG_A * (12.0 * x - HLG_B).ln() + HLG_C
    }
}

/// CPU reference for the shader's transfer switch. Used to populate
/// per-pixel test vectors when comparing GPU output to expected.
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

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1.5e-5;

    fn round_trip(id: TransferId) {
        // Sample 257 points across [0, 1] inclusive at the endpoints.
        for i in 0..=256 {
            let x = i as f32 / 256.0;
            let lin = to_linear(id, x);
            let back = from_linear(id, lin);
            let err = (back - x).abs();
            assert!(
                err < EPS,
                "{:?} round-trip x={x}: linear={lin}, back={back}, err={err}",
                id
            );
        }
    }

    #[test]
    fn srgb_round_trip() {
        round_trip(TransferId::Srgb);
    }

    #[test]
    fn bt709_round_trip() {
        round_trip(TransferId::Bt709);
    }

    #[test]
    fn pq_round_trip() {
        round_trip(TransferId::Pq);
    }

    #[test]
    fn hlg_round_trip() {
        round_trip(TransferId::Hlg);
    }

    #[test]
    fn linear_is_identity() {
        for &x in &[0.0_f32, 0.25, 0.5, 0.75, 1.0, 2.0, -0.1] {
            assert_eq!(to_linear(TransferId::Linear, x), x);
            assert_eq!(from_linear(TransferId::Linear, x), x);
        }
    }

    #[test]
    fn srgb_known_points() {
        // Mid-gray 0.5 sRGB → ~0.2140 linear (well-known reference).
        let lin = srgb_to_linear(0.5);
        assert!((lin - 0.2140).abs() < 1e-3, "expected ~0.2140, got {lin}");
        // Round-trip preserves it.
        let back = linear_to_srgb(lin);
        assert!((back - 0.5).abs() < 1e-5);
    }

    #[test]
    fn pq_peak_is_10000_nit_normalized() {
        // PQ encodes 10000 nit at encoded=1.0. Linearized, that's 1.0
        // (the canonical "1.0 = 10000 nit" reference).
        let lin = pq_to_linear(1.0);
        assert!((lin - 1.0).abs() < 1e-4);
    }

    #[test]
    fn hlg_half_point() {
        // HLG OETF transition at encoded=0.5: gamma curve below, log above.
        // f(0.5) = 0.25 / 3 = 0.08333…
        let lin = hlg_to_linear(0.5);
        assert!(
            (lin - 0.0833333).abs() < 1e-5,
            "expected ~0.0833, got {lin}"
        );
    }
}
