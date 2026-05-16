// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Closed-form transfer functions used by this crate's tone-curve math
//! and tests.
//!
//! Mirrors the constants in `libs/streamlib-engine/src/core/color/transfer.rs`
//! verbatim — the engine's transfer math is the canonical home today; this
//! file carries the subset (`TransferId`, PQ encode/decode, sRGB encode)
//! that `tone.rs` needs without forcing this package to depend on
//! engine-internal modules. When color math fully migrates out of the
//! engine, the engine's `transfer.rs` consolidates into this file (or
//! vice-versa).

/// Shader-side transfer-function identifier. The numeric value is
/// passed through the push-constant `input_transfer` / `output_transfer`
/// slots — the shader switches on it. Must stay in sync with the
/// `TRANSFER_*` constants in `src/shaders/color_common.glsl`.
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
