// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-wide color-management primitives.
//!
//! Engine core math + kernel inputs are engine-ID-shaped — every
//! type re-exported here uses [`PrimariesId`] / [`TransferId`] /
//! [`MatrixId`] / [`RangeId`] rather than the wire-format
//! `_generated_::tatolab__core::color_info::*` schema enums.
//! Schema↔engine-ID translation lives in [`translate`] and is the
//! only engine module that imports the schema types.

mod matrix;
mod resolve;
mod resolved;
mod tone;
mod transfer;
mod translate;

pub use matrix::{yuv_to_rgb_matrix, YuvToRgbDecomposition};
pub use resolve::resolve_color_defaults;
pub use resolved::{
    ColorSpaceKind, ColorTraits, HdrStaticMetadata, MatrixId, PrimariesId, RangeId,
    ResolvedColorInfo,
};
pub use tone::{bt2390_eetf_per_channel, bt2446a_inverse_per_channel};
pub use transfer::{
    bt709_to_linear, from_linear, hlg_to_linear, linear_to_bt709, linear_to_hlg, linear_to_pq,
    linear_to_srgb, pq_to_linear, srgb_to_linear, to_linear, TransferId,
};
pub use translate::{
    color_traits_from_color_info, hdr_metadata_from_schema, matrix_id_from_schema,
    primaries_id_from_schema, range_id_from_schema, transfer_id_from_schema,
};
