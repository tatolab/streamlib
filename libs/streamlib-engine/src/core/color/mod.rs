// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-wide color-management primitives.
//!
//! - [`ResolvedColorInfo`] — fully-resolved 4-axis color description.
//! - [`resolve_color_defaults`] — sparse [`crate::_generated_::ColorInfo`]
//!   → [`ResolvedColorInfo`] with per-kind defaults.
//! - [`yuv_to_rgb_matrix`] — 3×3 YCbCr→RGB matrix + range-domain offset.
//! - [`TransferId`] / [`to_linear`] / [`from_linear`] — closed-form
//!   transfer functions (sRGB, BT.709, PQ, HLG, Linear).

mod matrix;
mod resolve;
mod resolved;
mod tone;
mod transfer;

pub use matrix::{yuv_to_rgb_matrix, YuvToRgbDecomposition};
pub use resolve::resolve_color_defaults;
pub use resolved::{ColorSpaceKind, ResolvedColorInfo};
pub use tone::{bt2390_eetf_per_channel, bt2446a_inverse_per_channel};
pub use transfer::{
    bt709_to_linear, from_linear, hlg_to_linear, linear_to_bt709, linear_to_hlg, linear_to_pq,
    linear_to_srgb, pq_to_linear, srgb_to_linear, to_linear, TransferId,
};
