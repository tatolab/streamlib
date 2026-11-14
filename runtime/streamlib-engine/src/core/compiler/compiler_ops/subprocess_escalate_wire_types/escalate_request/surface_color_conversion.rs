// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// Convert between a YCbCr surface and an RGBA texture through the engine's
/// colour converter, matrix and range only, keeping the transfer curve: a
/// YCbCr pixel buffer into an RGBA texture, or on macOS an RGBA texture into
/// an NV12 IOSurface. The host records, submits and waits before it answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestConvertSurfaceInto {
    /// An `rgba8_unorm` texture allocated with `storage_binding` usage, or on
    /// macOS an adopted NV12 IOSurface, of the source's extent.
    pub(crate) destination_surface_id: String,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// The YCbCr side's colour, as `ColorInfo` axis names; an absent axis is
    /// unspecified and takes the engine's YCbCr default.
    pub(crate) ycbcr_side_color: EscalateRequestColorInfoAxisNames,

    /// A pixel buffer in `nv12_video_range`, `nv12_full_range` or `yuyv422`,
    /// laid out as its registration states, or on macOS an `rgba8_unorm`
    /// texture.
    pub(crate) source_surface_id: String,
}

/// A `ColorInfo` as its four axis names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestColorInfoAxisNames {
    /// `bt709`, `smpte170m`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) matrix: Option<String>,

    /// `bt709`, `bt2020`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) primaries: Option<String>,

    /// `full` or `limited`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) range: Option<String>,

    /// `bt709`, `srgb`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) transfer: Option<String>,
}
