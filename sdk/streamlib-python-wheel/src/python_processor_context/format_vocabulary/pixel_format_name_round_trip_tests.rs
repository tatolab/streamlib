// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::*;

/// Every format string the old SDK accepted still parses, and every
/// engine variant renders back to the string it parses from.
#[test]
fn pixel_format_names_round_trip() {
    for format in [
        PixelFormat::Bgra32,
        PixelFormat::Rgba32,
        PixelFormat::Argb32,
        PixelFormat::Rgba64,
        PixelFormat::Rgba16Float,
        PixelFormat::Rgba32Float,
        PixelFormat::Nv12VideoRange,
        PixelFormat::Nv12FullRange,
        PixelFormat::Uyvy422,
        PixelFormat::Yuyv422,
        PixelFormat::Gray8,
    ] {
        assert_eq!(parse_pixel_format_name(format.wire_name()).unwrap(), format);
    }
    assert_eq!(
        parse_pixel_format_name("bgra").unwrap(),
        PixelFormat::Bgra32,
        "the old SDK's default mnemonic must keep working"
    );
    assert!(parse_pixel_format_name("sepia").is_err());
}
