// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Format converter for pixel buffer format conversion.
//!
//! RhiFormatConverter is a stateless "recipe" that wraps the platform's
//! format conversion API (vImageConverter on macOS). It defines HOW to
//! convert between two formats, not WHERE the data is.
//!
//! The converter is thread-safe and can be used concurrently from multiple
//! processors without any locking.

use super::{PixelFormat, RhiPixelBuffer};
use crate::core::Result;

/// Stateless format converter - a "recipe" for pixel format conversion.
///
/// The converter is created once for a (source_format, dest_format) pair,
/// then reused for all conversions between those formats. The processor
/// is responsible for providing source and destination buffers.
///
/// Thread-safe: can be shared across processors and used concurrently.
pub struct RhiFormatConverter {
    #[cfg(target_os = "macos")]
    pub(crate) inner: crate::metal::rhi::format_converter::FormatConverterMacOS,

    #[cfg(not(target_os = "macos"))]
    _marker: std::marker::PhantomData<()>,
}

impl RhiFormatConverter {
    /// Create a new converter for the given format pair.
    ///
    /// The converter can be reused for any number of conversions between
    /// these formats.
    pub fn new(source_format: PixelFormat, dest_format: PixelFormat) -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            use crate::metal::rhi::format_converter::FormatConverterMacOS;
            Ok(Self {
                inner: FormatConverterMacOS::new(source_format, dest_format)?,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (source_format, dest_format);
            Err(crate::core::StreamError::Configuration(
                "RhiFormatConverter not implemented for this platform".into(),
            ))
        }
    }

    /// Get the source format this converter accepts.
    pub fn source_format(&self) -> PixelFormat {
        #[cfg(target_os = "macos")]
        {
            self.inner.source_format()
        }
        #[cfg(not(target_os = "macos"))]
        {
            PixelFormat::Unknown
        }
    }

    /// Get the destination format this converter produces.
    pub fn dest_format(&self) -> PixelFormat {
        #[cfg(target_os = "macos")]
        {
            self.inner.dest_format()
        }
        #[cfg(not(target_os = "macos"))]
        {
            PixelFormat::Unknown
        }
    }

    /// Convert pixel data from source buffer to destination buffer.
    ///
    /// Both buffers must have the same dimensions. The source must match
    /// the converter's source format, and the destination must match
    /// the converter's destination format.
    ///
    /// This is thread-safe and can be called concurrently from multiple
    /// processors - no internal locking is performed.
    pub fn convert(&self, source: &RhiPixelBuffer, dest: &RhiPixelBuffer) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.inner.convert(source, dest)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (source, dest);
            Err(crate::core::StreamError::Configuration(
                "RhiFormatConverter not implemented for this platform".into(),
            ))
        }
    }
}

impl std::fmt::Debug for RhiFormatConverter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiFormatConverter")
            .field("source", &self.source_format())
            .field("dest", &self.dest_format())
            .finish()
    }
}

// vImageConverter is thread-safe for concurrent use.
// The converter is read-only after creation.
unsafe impl Send for RhiFormatConverter {}
unsafe impl Sync for RhiFormatConverter {}
