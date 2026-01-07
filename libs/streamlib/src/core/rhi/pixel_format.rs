// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pixel format for video buffers.
//!
//! On macOS/iOS, enum values ARE CVPixelFormatType constants directly.
//! This ensures zero-cost conversion to platform APIs.

// Platform-specific pixel format definitions
#[cfg(target_os = "macos")]
mod platform {
    /// Pixel format backed directly by CVPixelFormatType constants.
    ///
    /// Values are the exact CVPixelFormatType FourCC codes from CoreVideo.
    /// No conversion needed - cast directly to u32 for CoreVideo APIs.
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
        /// Get the raw CVPixelFormatType value.
        #[inline]
        pub const fn as_cv_pixel_format_type(&self) -> u32 {
            *self as u32
        }

        /// Create from CVPixelFormatType value.
        pub fn from_cv_pixel_format_type(cv_format: u32) -> Self {
            match cv_format {
                0x42475241 => Self::Bgra32,
                0x52474241 => Self::Rgba32,
                0x00000020 => Self::Argb32,
                0x52476841 => Self::Rgba64,
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
                Self::Bgra32 | Self::Rgba32 | Self::Argb32 | Self::Rgba64
            )
        }

        /// Bits per pixel for this format.
        pub const fn bits_per_pixel(&self) -> u32 {
            match self {
                Self::Bgra32 | Self::Rgba32 | Self::Argb32 => 32,
                Self::Rgba64 => 64,
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
                Self::Rgba64 => 16,
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
                Self::Uyvy422 | Self::Yuyv422 => 1,
                Self::Nv12VideoRange | Self::Nv12FullRange => 2,
                Self::Gray8 => 1,
                Self::Unknown => 1,
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
}

// Re-export platform-specific type
#[cfg(target_os = "macos")]
pub use platform::PixelFormat;
