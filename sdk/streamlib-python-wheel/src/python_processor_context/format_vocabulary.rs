// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use streamlib::sdk::rhi::PixelFormat;

/// Parse a Python-facing format string, mapping the refusal into the
/// `ValueError` Python expects.
pub(crate) fn parse_pixel_format_name(name: &str) -> PyResult<PixelFormat> {
    PixelFormat::parse_wire_name(name).map_err(PyValueError::new_err)
}

/// Texture formats travel to the parent as their wire spelling, which the host
/// parses. Validating the spelling here keeps the refusal on the caller's own
/// stack rather than arriving as an escalate failure.
const TEXTURE_FORMAT_WIRE_NAMES: &[&str] = &[
    "bgra8_unorm",
    "bgra8_unorm_srgb",
    "r8_unorm",
    "rg8_unorm",
    "rgba8_unorm",
    "rgba8_unorm_srgb",
    "rgba16_float",
    "rgba32_float",
];

pub(super) fn parse_texture_format_name(name: &str) -> PyResult<&'static str> {
    TEXTURE_FORMAT_WIRE_NAMES
        .iter()
        .find(|known| **known == name)
        .copied()
        .ok_or_else(|| {
            PyValueError::new_err(format!(
                "unknown texture format {name:?}; the formats a texture can be acquired in are \
                 {}",
                TEXTURE_FORMAT_WIRE_NAMES.join(", ")
            ))
        })
}

#[cfg(test)]
mod pixel_format_name_round_trip_tests;
