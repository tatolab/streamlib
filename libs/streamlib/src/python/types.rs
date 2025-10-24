//! Python bindings for streamlib core types

use pyo3::prelude::*;
use crate::core::VideoFrame;
use std::collections::HashMap;

/// Python wrapper for VideoFrame
///
/// Represents a video frame with GPU texture data.
/// Python code sees this as an opaque handle - the actual GPU memory stays on GPU.
#[pyclass(name = "VideoFrame")]
#[derive(Clone)]
pub struct PyVideoFrame {
    /// The underlying Rust VideoFrame
    pub(crate) inner: VideoFrame,
}

#[pymethods]
impl PyVideoFrame {
    /// Get frame width
    #[getter]
    fn width(&self) -> u32 {
        self.inner.width
    }

    /// Get frame height
    #[getter]
    fn height(&self) -> u32 {
        self.inner.height
    }

    /// Get frame format as string
    #[getter]
    fn format(&self) -> String {
        format!("{:?}", self.inner.format)
    }

    /// Get frame timestamp
    #[getter]
    fn timestamp(&self) -> f64 {
        self.inner.timestamp
    }

    /// Get frame number
    #[getter]
    fn frame_number(&self) -> u64 {
        self.inner.frame_number
    }

    /// Get metadata as dictionary
    #[getter]
    fn metadata(&self) -> HashMap<String, String> {
        // Convert Option<HashMap<String, MetadataValue>> to HashMap<String, String>
        self.inner.metadata.as_ref()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), format!("{:?}", v)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// String representation
    fn __repr__(&self) -> String {
        format!(
            "VideoFrame({}x{}, {:?}, frame={})",
            self.inner.width, self.inner.height, self.inner.format, self.inner.frame_number
        )
    }

    /// String representation
    fn __str__(&self) -> String {
        self.__repr__()
    }
}

impl PyVideoFrame {
    /// Create from Rust VideoFrame
    pub fn from_rust(frame: VideoFrame) -> Self {
        Self { inner: frame }
    }

    /// Get reference to inner Rust VideoFrame
    pub fn as_rust(&self) -> &VideoFrame {
        &self.inner
    }

    /// Convert to owned Rust VideoFrame
    pub fn into_rust(self) -> VideoFrame {
        self.inner
    }
}
