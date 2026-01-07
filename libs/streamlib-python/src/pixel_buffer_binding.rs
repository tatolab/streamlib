// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for RhiPixelBuffer and PixelFormat.

use pyo3::prelude::*;
use streamlib::core::rhi::{PixelFormat, RhiPixelBuffer};

// =============================================================================
// PixelFormat enum - 1:1 mapping with Rust PixelFormat
// =============================================================================

/// Pixel format for video buffers.
///
/// This enum maps exactly to the Rust PixelFormat enum.
/// Values correspond to CVPixelFormatType constants on macOS.
#[pyclass(name = "PixelFormat", eq, eq_int)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PyPixelFormat {
    /// 32-bit BGRA (8 bits/channel). Most common format on macOS.
    Bgra32,
    /// 32-bit RGBA (8 bits/channel).
    Rgba32,
    /// 32-bit ARGB (8 bits/channel).
    Argb32,
    /// 64-bit RGBA (16 bits/channel).
    Rgba64,
    /// NV12 YUV 4:2:0 bi-planar, video range.
    Nv12VideoRange,
    /// NV12 YUV 4:2:0 bi-planar, full range.
    Nv12FullRange,
    /// UYVY packed YUV 4:2:2.
    Uyvy422,
    /// YUYV packed YUV 4:2:2.
    Yuyv422,
    /// 8-bit grayscale.
    Gray8,
    /// Unknown or unsupported format.
    Unknown,
}

impl From<PixelFormat> for PyPixelFormat {
    fn from(format: PixelFormat) -> Self {
        match format {
            PixelFormat::Bgra32 => PyPixelFormat::Bgra32,
            PixelFormat::Rgba32 => PyPixelFormat::Rgba32,
            PixelFormat::Argb32 => PyPixelFormat::Argb32,
            PixelFormat::Rgba64 => PyPixelFormat::Rgba64,
            PixelFormat::Nv12VideoRange => PyPixelFormat::Nv12VideoRange,
            PixelFormat::Nv12FullRange => PyPixelFormat::Nv12FullRange,
            PixelFormat::Uyvy422 => PyPixelFormat::Uyvy422,
            PixelFormat::Yuyv422 => PyPixelFormat::Yuyv422,
            PixelFormat::Gray8 => PyPixelFormat::Gray8,
            PixelFormat::Unknown => PyPixelFormat::Unknown,
        }
    }
}

impl From<PyPixelFormat> for PixelFormat {
    fn from(format: PyPixelFormat) -> Self {
        match format {
            PyPixelFormat::Bgra32 => PixelFormat::Bgra32,
            PyPixelFormat::Rgba32 => PixelFormat::Rgba32,
            PyPixelFormat::Argb32 => PixelFormat::Argb32,
            PyPixelFormat::Rgba64 => PixelFormat::Rgba64,
            PyPixelFormat::Nv12VideoRange => PixelFormat::Nv12VideoRange,
            PyPixelFormat::Nv12FullRange => PixelFormat::Nv12FullRange,
            PyPixelFormat::Uyvy422 => PixelFormat::Uyvy422,
            PyPixelFormat::Yuyv422 => PixelFormat::Yuyv422,
            PyPixelFormat::Gray8 => PixelFormat::Gray8,
            PyPixelFormat::Unknown => PixelFormat::Unknown,
        }
    }
}

// =============================================================================
// PyRhiPixelBuffer
// =============================================================================

/// Python-accessible pixel buffer wrapper.
///
/// RhiPixelBuffer wraps a platform pixel buffer (CVPixelBuffer on macOS).
/// Clone is cheap - just increments the platform refcount.
#[pyclass(name = "PixelBuffer")]
#[derive(Clone)]
pub struct PyRhiPixelBuffer {
    inner: RhiPixelBuffer,
}

impl PyRhiPixelBuffer {
    pub fn new(buffer: RhiPixelBuffer) -> Self {
        Self { inner: buffer }
    }

    pub fn inner(&self) -> &RhiPixelBuffer {
        &self.inner
    }

    pub fn into_inner(self) -> RhiPixelBuffer {
        self.inner
    }
}

#[pymethods]
impl PyRhiPixelBuffer {
    /// Width in pixels.
    #[getter]
    fn width(&self) -> u32 {
        self.inner.width
    }

    /// Height in pixels.
    #[getter]
    fn height(&self) -> u32 {
        self.inner.height
    }

    /// Pixel format.
    #[getter]
    fn format(&self) -> PyPixelFormat {
        self.inner.format().into()
    }

    fn __repr__(&self) -> String {
        format!(
            "PixelBuffer({}x{}, format={:?})",
            self.inner.width,
            self.inner.height,
            PyPixelFormat::from(self.inner.format())
        )
    }
}
