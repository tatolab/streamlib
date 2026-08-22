// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pixel format for video buffers.
//!
//! On macOS/iOS, enum values are CVPixelFormatType constants wherever
//! CoreVideo has one free — [`PixelFormat::Rgba16Float`] is the one
//! StreamLib-local code, so conversion goes through
//! `as_cv_pixel_format_type`, never a bare cast.

/// Pixel format backed by CVPixelFormatType constants.
///
/// Values are the exact CVPixelFormatType FourCC codes from CoreVideo,
/// except [`Self::Rgba16Float`] — CoreVideo's half-float code is
/// occupied by [`Self::Rgba64`] — so CoreVideo APIs take
/// `as_cv_pixel_format_type()`, which maps that one variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum PixelFormat {
    // ===========================================
    // 8-bit RGB formats (32 bits per pixel)
    // ===========================================
    /// 32-bit BGRA (8 bits/channel). kCVPixelFormatType_32BGRA = 'BGRA'
    #[default]
    Bgra32 = 0x42475241,
    /// 32-bit RGBA (8 bits/channel). kCVPixelFormatType_32RGBA = 'RGBA'
    Rgba32 = 0x52474241,
    /// 32-bit ARGB (8 bits/channel). kCVPixelFormatType_32ARGB = 32
    Argb32 = 0x00000020,

    // ===========================================
    // 16-bit RGB formats (64 bits per pixel)
    // ===========================================
    /// 64-bit RGBA little-endian (16 bits/channel). kCVPixelFormatType_64RGBALE = 'RGhA'
    Rgba64 = 0x52476841,

    // ===========================================
    // Float RGB formats (texture-backed exports)
    // ===========================================
    /// 64-bit RGBA half-float (16 bits/channel). StreamLib-local code
    /// 'RGhF': CoreVideo's half-float code 'RGhA' is already taken by
    /// [`Self::Rgba64`] above, whose consumers treat it as uint16.
    Rgba16Float = 0x52476846,
    /// 128-bit RGBA float (32 bits/channel). kCVPixelFormatType_128RGBAFloat = 'RGfA'
    Rgba32Float = 0x52476641,

    // ===========================================
    // YUV formats
    // ===========================================
    /// NV12 YUV 4:2:0 bi-planar, video range. kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange = '420v'
    Nv12VideoRange = 0x34323076,
    /// NV12 YUV 4:2:0 bi-planar, full range. kCVPixelFormatType_420YpCbCr8BiPlanarFullRange = '420f'
    Nv12FullRange = 0x34323066,
    /// UYVY packed YUV 4:2:2. kCVPixelFormatType_422YpCbCr8 = '2vuy'
    Uyvy422 = 0x32767579,
    /// YUYV packed YUV 4:2:2. kCVPixelFormatType_422YpCbCr8_yuvs = 'yuvs'
    Yuyv422 = 0x79757673,

    // ===========================================
    // Grayscale / single channel
    // ===========================================
    /// 8-bit grayscale. kCVPixelFormatType_OneComponent8 = 'L008'
    Gray8 = 0x4C303038,

    /// Unknown or unsupported format.
    Unknown = 0x00000000,
}

impl PixelFormat {
    /// The CVPixelFormatType value CoreVideo knows this format by.
    ///
    /// Identity for every variant except [`Self::Rgba16Float`], whose
    /// discriminant is StreamLib-local — CoreVideo spells half-float
    /// RGBA 'RGhA' (kCVPixelFormatType_64RGBAHalf), a code
    /// [`Self::Rgba64`]'s discriminant already occupies, so a bare cast
    /// would hand CoreVideo an OSType it does not know.
    #[cfg(target_os = "macos")]
    #[inline]
    pub const fn as_cv_pixel_format_type(&self) -> u32 {
        match self {
            Self::Rgba16Float => 0x52476841,
            _ => *self as u32,
        }
    }

    /// Create from CVPixelFormatType value.
    ///
    /// Not the inverse of [`Self::as_cv_pixel_format_type`] for
    /// [`Self::Rgba16Float`]: both it and [`Self::Rgba64`] map onto
    /// CoreVideo's 'RGhA', and this direction resolves that code to
    /// `Rgba64` — a caller that must preserve float identity across
    /// CoreVideo carries the `PixelFormat` itself, not the OSType.
    #[cfg(target_os = "macos")]
    pub fn from_cv_pixel_format_type(cv_format: u32) -> Self {
        match cv_format {
            0x42475241 => Self::Bgra32,
            0x52474241 => Self::Rgba32,
            0x00000020 => Self::Argb32,
            0x52476841 => Self::Rgba64,
            0x52476846 => Self::Rgba16Float,
            0x52476641 => Self::Rgba32Float,
            0x34323076 => Self::Nv12VideoRange,
            0x34323066 => Self::Nv12FullRange,
            0x32767579 => Self::Uyvy422,
            0x79757673 => Self::Yuyv422,
            0x4C303038 => Self::Gray8,
            _ => Self::Unknown,
        }
    }

    /// Whether this is a YUV format.
    pub const fn is_yuv(&self) -> bool {
        matches!(
            self,
            Self::Nv12VideoRange | Self::Nv12FullRange | Self::Uyvy422 | Self::Yuyv422
        )
    }

    /// Whether this is an RGB format.
    pub const fn is_rgb(&self) -> bool {
        matches!(
            self,
            Self::Bgra32
                | Self::Rgba32
                | Self::Argb32
                | Self::Rgba64
                | Self::Rgba16Float
                | Self::Rgba32Float
        )
    }

    /// Bits per pixel for this format.
    pub const fn bits_per_pixel(&self) -> u32 {
        match self {
            Self::Bgra32 | Self::Rgba32 | Self::Argb32 => 32,
            Self::Rgba64 | Self::Rgba16Float => 64,
            Self::Rgba32Float => 128,
            Self::Nv12VideoRange | Self::Nv12FullRange => 12, // Average for 4:2:0
            Self::Uyvy422 | Self::Yuyv422 => 16,
            Self::Gray8 => 8,
            Self::Unknown => 0,
        }
    }

    /// Bits per component (channel) for this format.
    pub const fn bits_per_component(&self) -> u32 {
        match self {
            Self::Bgra32 | Self::Rgba32 | Self::Argb32 => 8,
            Self::Rgba64 | Self::Rgba16Float => 16,
            Self::Rgba32Float => 32,
            Self::Nv12VideoRange | Self::Nv12FullRange => 8,
            Self::Uyvy422 | Self::Yuyv422 => 8,
            Self::Gray8 => 8,
            Self::Unknown => 0,
        }
    }

    /// Number of planes for this format.
    pub const fn plane_count(&self) -> u32 {
        match self {
            Self::Bgra32 | Self::Rgba32 | Self::Argb32 | Self::Rgba64 => 1,
            Self::Rgba16Float | Self::Rgba32Float => 1,
            Self::Uyvy422 | Self::Yuyv422 => 1,
            Self::Nv12VideoRange | Self::Nv12FullRange => 2,
            Self::Gray8 => 1,
            Self::Unknown => 1,
        }
    }

    /// The one wire spelling of this format: pixel-buffer surface-share
    /// registration metadata, escalate requests, and the Python-facing
    /// format strings speak exactly this vocabulary. Lowercase snake-case
    /// of the variant. (Texture registrations carry `TextureFormat`
    /// spellings, a separate vocabulary.)
    pub const fn wire_name(&self) -> &'static str {
        match self {
            Self::Bgra32 => "bgra32",
            Self::Rgba32 => "rgba32",
            Self::Argb32 => "argb32",
            Self::Rgba64 => "rgba64",
            Self::Rgba16Float => "rgba16_float",
            Self::Rgba32Float => "rgba32_float",
            Self::Nv12VideoRange => "nv12_video_range",
            Self::Nv12FullRange => "nv12_full_range",
            Self::Uyvy422 => "uyvy422",
            Self::Yuyv422 => "yuyv422",
            Self::Gray8 => "gray8",
            Self::Unknown => "unknown",
        }
    }

    /// Parse the wire spelling, plus the shorthand aliases the authoring
    /// surfaces accept (`"bgra"`, `"nv12"`, …). Case-insensitive.
    pub fn parse_wire_name(name: &str) -> Result<Self, String> {
        let normalized = name.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "bgra" | "bgra32" => Ok(Self::Bgra32),
            "rgba" | "rgba32" => Ok(Self::Rgba32),
            "argb" | "argb32" => Ok(Self::Argb32),
            "rgba64" => Ok(Self::Rgba64),
            "rgba16_float" => Ok(Self::Rgba16Float),
            "rgba32_float" => Ok(Self::Rgba32Float),
            "nv12" | "nv12_video_range" => Ok(Self::Nv12VideoRange),
            "nv12_full_range" => Ok(Self::Nv12FullRange),
            "uyvy" | "uyvy422" => Ok(Self::Uyvy422),
            "yuyv" | "yuyv422" => Ok(Self::Yuyv422),
            "gray" | "gray8" => Ok(Self::Gray8),
            unknown => Err(format!("unknown pixel format '{unknown}'")),
        }
    }

    /// FourCC string representation for debugging.
    pub fn fourcc_string(&self) -> String {
        let code = *self as u32;
        if code < 256 {
            // Numeric format (like 32 for ARGB)
            return format!("{}", code);
        }
        let bytes = code.to_be_bytes();
        bytes
            .iter()
            .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
            .collect()
    }
}

#[cfg(test)]
mod layout_tests {
    //! Layout regression tests for the FFI-crossing pixel-format
    //! primitive.
    //!
    //! `#[repr(u32)]` so the enum is byte-equivalent to a bare `u32`
    //! across the plugin FFI boundary — adapter vtables in
    //! `streamlib-adapter-cpu-readback` (and elsewhere) carry
    //! `format_raw: u32` arguments that round-trip through
    //! [`PixelFormat`] via an `as` cast. Discriminant values are
    //! CVPixelFormatType FourCC constants (bar the StreamLib-local
    //! `Rgba16Float`) — pinning them locks against silent re-numbering.
    use super::*;
    use core::mem::{align_of, size_of};

    #[test]
    fn pixel_format_layout() {
        assert_eq!(size_of::<PixelFormat>(), 4);
        assert_eq!(align_of::<PixelFormat>(), 4);
    }

    #[test]
    fn pixel_format_discriminants_are_pinned() {
        // CVPixelFormatType FourCC constants. Locked: a silent change
        // in any of these silently re-maps cdylibs' format payloads
        // onto the wrong host-side variant.
        assert_eq!(PixelFormat::Bgra32 as u32, 0x42475241);
        assert_eq!(PixelFormat::Rgba32 as u32, 0x52474241);
        assert_eq!(PixelFormat::Argb32 as u32, 0x00000020);
        assert_eq!(PixelFormat::Rgba64 as u32, 0x52476841);
        assert_eq!(PixelFormat::Rgba16Float as u32, 0x52476846);
        assert_eq!(PixelFormat::Rgba32Float as u32, 0x52476641);
        assert_eq!(PixelFormat::Nv12VideoRange as u32, 0x34323076);
        assert_eq!(PixelFormat::Nv12FullRange as u32, 0x34323066);
        assert_eq!(PixelFormat::Uyvy422 as u32, 0x32767579);
        assert_eq!(PixelFormat::Yuyv422 as u32, 0x79757673);
        assert_eq!(PixelFormat::Gray8 as u32, 0x4C303038);
        assert_eq!(PixelFormat::Unknown as u32, 0x00000000);
    }

    #[test]
    fn pixel_format_default_is_bgra32() {
        // The `Default` impl IS part of the wire contract for FFI
        // payloads that default-initialize a buffer of `PixelFormat`.
        assert_eq!(PixelFormat::default() as u32, PixelFormat::Bgra32 as u32);
    }

    #[test]
    fn every_wire_name_parses_back_to_its_format() {
        // The wire vocabulary is one definition used on both sides of the
        // surface-share and escalate protocols; a name that does not round
        // trip would import a checked-out surface under the wrong format.
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
            assert_eq!(PixelFormat::parse_wire_name(format.wire_name()), Ok(format));
        }
        assert!(PixelFormat::parse_wire_name("unknown").is_err());
        assert!(PixelFormat::parse_wire_name("not-a-format").is_err());
    }
}
