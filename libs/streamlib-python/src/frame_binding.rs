// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unified frame wrapper for all schema types.
//!
//! Provides a single `PyFrame` type that wraps VideoFrame, AudioFrame, or DataFrame,
//! exposing a consistent `get()` interface to Python code.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;
use streamlib::{AudioFrame, DataFrame, VideoFrame};

use crate::schema_field_mappers::{
    audio_frame_field_to_py, audio_frame_to_dict, data_frame_field_to_py, data_frame_to_dict,
    video_frame_field_to_py, video_frame_to_dict,
};

/// Inner enum holding the actual frame data.
#[derive(Clone)]
pub enum FrameInner {
    Video(VideoFrame),
    Audio(AudioFrame),
    Data(DataFrame),
}

/// Unified frame wrapper for Python.
///
/// Wraps VideoFrame, AudioFrame, or DataFrame with a consistent interface.
/// Use `get()` to retrieve the whole frame as a dict, or `get("field")` for a specific field.
#[pyclass(name = "Frame")]
#[derive(Clone)]
pub struct PyFrame {
    inner: Arc<FrameInner>,
    schema_name: String,
}

impl PyFrame {
    /// Create a new PyFrame wrapping a VideoFrame.
    pub fn from_video_frame(frame: VideoFrame) -> Self {
        Self {
            inner: Arc::new(FrameInner::Video(frame)),
            schema_name: "VideoFrame".to_string(),
        }
    }

    /// Create a new PyFrame wrapping an AudioFrame.
    pub fn from_audio_frame(frame: AudioFrame) -> Self {
        Self {
            inner: Arc::new(FrameInner::Audio(frame)),
            schema_name: "AudioFrame".to_string(),
        }
    }

    /// Create a new PyFrame wrapping a DataFrame.
    pub fn from_data_frame(frame: DataFrame) -> Self {
        Self {
            inner: Arc::new(FrameInner::Data(frame)),
            schema_name: "DataFrame".to_string(),
        }
    }

    /// Get the inner FrameInner enum.
    pub fn inner(&self) -> &FrameInner {
        &self.inner
    }

    /// Get the schema name.
    pub fn schema_name(&self) -> &str {
        &self.schema_name
    }

    /// Try to get as VideoFrame.
    pub fn as_video_frame(&self) -> Option<&VideoFrame> {
        match self.inner.as_ref() {
            FrameInner::Video(f) => Some(f),
            _ => None,
        }
    }

    /// Try to get as AudioFrame.
    pub fn as_audio_frame(&self) -> Option<&AudioFrame> {
        match self.inner.as_ref() {
            FrameInner::Audio(f) => Some(f),
            _ => None,
        }
    }

    /// Try to get as DataFrame.
    pub fn as_data_frame(&self) -> Option<&DataFrame> {
        match self.inner.as_ref() {
            FrameInner::Data(f) => Some(f),
            _ => None,
        }
    }
}

impl PyFrame {
    /// Get the whole frame as a dict, or a specific field by name (Rust API).
    pub fn get_field(&self, py: Python<'_>, field: Option<&str>) -> PyResult<Py<PyAny>> {
        match field {
            None => {
                // Return whole frame as dict
                match self.inner.as_ref() {
                    FrameInner::Video(f) => Ok(video_frame_to_dict(py, f)?.into_any()),
                    FrameInner::Audio(f) => Ok(audio_frame_to_dict(py, f)?.into_any()),
                    FrameInner::Data(f) => Ok(data_frame_to_dict(py, f)?.into_any()),
                }
            }
            Some(field_name) => {
                // Return specific field
                match self.inner.as_ref() {
                    FrameInner::Video(f) => video_frame_field_to_py(py, f, field_name),
                    FrameInner::Audio(f) => audio_frame_field_to_py(py, f, field_name),
                    FrameInner::Data(f) => data_frame_field_to_py(py, f, field_name),
                }
            }
        }
    }
}

#[pymethods]
impl PyFrame {
    /// Get the whole frame as a dict, or a specific field by name.
    ///
    /// Usage:
    ///   frame.get()          # Returns dict with all fields
    ///   frame.get("width")   # Returns just the width field
    #[pyo3(signature = (field=None))]
    fn get(&self, py: Python<'_>, field: Option<&str>) -> PyResult<Py<PyAny>> {
        match field {
            None => {
                // Return whole frame as dict
                match self.inner.as_ref() {
                    FrameInner::Video(f) => Ok(video_frame_to_dict(py, f)?.into_any()),
                    FrameInner::Audio(f) => Ok(audio_frame_to_dict(py, f)?.into_any()),
                    FrameInner::Data(f) => Ok(data_frame_to_dict(py, f)?.into_any()),
                }
            }
            Some(field_name) => {
                // Return specific field
                match self.inner.as_ref() {
                    FrameInner::Video(f) => video_frame_field_to_py(py, f, field_name),
                    FrameInner::Audio(f) => audio_frame_field_to_py(py, f, field_name),
                    FrameInner::Data(f) => data_frame_field_to_py(py, f, field_name),
                }
            }
        }
    }

    /// Get the schema name for this frame.
    #[getter]
    fn schema(&self) -> &str {
        &self.schema_name
    }

    fn __repr__(&self) -> String {
        match self.inner.as_ref() {
            FrameInner::Video(f) => {
                format!(
                    "Frame(schema='VideoFrame', width={}, height={}, frame_number={})",
                    f.width(),
                    f.height(),
                    f.frame_number
                )
            }
            FrameInner::Audio(f) => {
                format!(
                    "Frame(schema='AudioFrame', channels={}, sample_rate={}, frame_number={})",
                    f.channels.as_usize(),
                    f.sample_rate,
                    f.frame_number
                )
            }
            FrameInner::Data(f) => {
                format!(
                    "Frame(schema='DataFrame', data_len={}, timestamp_ns={})",
                    f.data.len(),
                    f.timestamp_ns
                )
            }
        }
    }
}

/// Build a VideoFrame from a Python dict.
///
/// Expected dict keys:
/// - `pixel_buffer`: PyRhiPixelBuffer - the underlying pixel buffer
/// - `timestamp_ns`: i64 - monotonic timestamp in nanoseconds
/// - `frame_number`: u64 - sequential frame number
pub fn video_frame_from_dict(
    _py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    _gpu_context: &streamlib::GpuContext,
) -> PyResult<VideoFrame> {
    use crate::pixel_buffer_binding::PyRhiPixelBuffer;

    // Extract pixel buffer (required)
    let pixel_buffer: PyRhiPixelBuffer = dict
        .get_item("pixel_buffer")?
        .ok_or_else(|| {
            pyo3::exceptions::PyKeyError::new_err(
                "VideoFrame dict requires 'pixel_buffer' field with a PixelBuffer",
            )
        })?
        .extract()?;

    // Extract timestamp (required)
    let timestamp_ns: i64 = dict
        .get_item("timestamp_ns")?
        .ok_or_else(|| {
            pyo3::exceptions::PyKeyError::new_err("VideoFrame dict requires 'timestamp_ns' field")
        })?
        .extract()?;

    // Extract frame number (required)
    let frame_number: u64 = dict
        .get_item("frame_number")?
        .ok_or_else(|| {
            pyo3::exceptions::PyKeyError::new_err("VideoFrame dict requires 'frame_number' field")
        })?
        .extract()?;

    // Create VideoFrame from the pixel buffer
    let buffer = pixel_buffer.into_inner();
    Ok(VideoFrame::from_buffer(buffer, timestamp_ns, frame_number))
}
