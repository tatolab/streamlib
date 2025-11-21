//! Python frame type wrappers
//!
//! Clippy false positive: useless_conversion warnings are from PyO3 macro expansion
#![allow(clippy::useless_conversion)]

use crate::core::{AudioFrame, DataFrame, VideoFrame};
use pyo3::prelude::*;
use std::collections::HashMap;

#[pyclass(name = "VideoFrame")]
#[derive(Clone)]
pub struct PyVideoFrame {
    pub(crate) inner: VideoFrame,
}

#[pymethods]
impl PyVideoFrame {
    #[getter]
    fn width(&self) -> u32 {
        self.inner.width
    }

    #[getter]
    fn height(&self) -> u32 {
        self.inner.height
    }

    #[getter]
    fn format(&self) -> String {
        format!("{:?}", self.inner.format)
    }

    #[getter]
    fn timestamp(&self) -> f64 {
        self.inner.timestamp_ns as f64 / 1_000_000_000.0
    }

    #[getter]
    fn frame_number(&self) -> u64 {
        self.inner.frame_number
    }

    #[getter]
    fn data(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        use super::gpu_wrappers::PyWgpuTexture;
        let texture_wrapper = PyWgpuTexture {
            texture: self.inner.texture.clone(),
        };
        Ok(Py::new(py, texture_wrapper)?.into_py(py))
    }

    fn clone_with_texture(
        &self,
        py: Python<'_>,
        new_texture: &Bound<'_, PyAny>,
    ) -> PyResult<PyVideoFrame> {
        use super::gpu_wrappers::PyWgpuTexture;

        let texture_wrapper: Py<PyWgpuTexture> = new_texture.extract()?;
        let texture_arc = texture_wrapper.borrow(py).texture.clone();

        let new_frame = VideoFrame {
            texture: texture_arc,
            format: self.inner.format,
            width: self.inner.width,
            height: self.inner.height,
            timestamp_ns: self.inner.timestamp_ns,
            frame_number: self.inner.frame_number,
            metadata: self.inner.metadata.clone(),
        };

        Ok(PyVideoFrame { inner: new_frame })
    }

    #[getter]
    fn metadata(&self) -> HashMap<String, String> {
        self.inner
            .metadata
            .as_ref()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), format!("{:?}", v)))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn __repr__(&self) -> String {
        format!(
            "VideoFrame({}x{}, {:?}, frame={})",
            self.inner.width, self.inner.height, self.inner.format, self.inner.frame_number
        )
    }

    fn __str__(&self) -> String {
        self.__repr__()
    }
}

impl PyVideoFrame {
    pub fn from_rust(frame: VideoFrame) -> Self {
        Self { inner: frame }
    }

    pub fn as_rust(&self) -> &VideoFrame {
        &self.inner
    }

    pub fn into_rust(self) -> VideoFrame {
        self.inner
    }
}

// ========== AudioFrame Wrapper ==========

#[pyclass(name = "AudioFrame")]
#[derive(Clone)]
pub struct PyAudioFrame {
    pub(crate) inner: AudioFrame,
}

#[pymethods]
impl PyAudioFrame {
    #[getter]
    fn channels(&self) -> usize {
        self.inner.channels.as_usize()
    }

    #[getter]
    fn sample_count(&self) -> usize {
        self.inner.sample_count()
    }

    #[getter]
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate
    }

    #[getter]
    fn timestamp_ns(&self) -> i64 {
        self.inner.timestamp_ns
    }

    #[getter]
    fn timestamp(&self) -> f64 {
        self.inner.timestamp_ns as f64 / 1_000_000_000.0
    }

    #[getter]
    fn frame_number(&self) -> u64 {
        self.inner.frame_number
    }

    /// Get interleaved sample data as a flat list of f32 values
    #[getter]
    fn samples(&self) -> Vec<f32> {
        (*self.inner.samples).clone()
    }

    /// Get samples for a specific channel (0-indexed)
    fn get_channel(&self, channel: usize) -> PyResult<Vec<f32>> {
        let channels = self.inner.channels.as_usize();
        if channel >= channels {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "Channel {} out of range (0-{})",
                channel,
                channels - 1
            )));
        }

        let sample_count = self.inner.sample_count();
        let mut channel_samples = Vec::with_capacity(sample_count);

        for i in 0..sample_count {
            channel_samples.push(self.inner.samples[i * channels + channel]);
        }

        Ok(channel_samples)
    }

    #[getter]
    fn metadata(&self) -> HashMap<String, String> {
        self.inner
            .metadata
            .as_ref()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), format!("{:?}", v)))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn __repr__(&self) -> String {
        format!(
            "AudioFrame<{}>({} samples @ {}Hz, frame={})",
            self.inner.channels.as_usize(),
            self.inner.sample_count(),
            self.inner.sample_rate,
            self.inner.frame_number
        )
    }

    fn __str__(&self) -> String {
        self.__repr__()
    }

    fn __len__(&self) -> usize {
        self.inner.sample_count()
    }
}

impl PyAudioFrame {
    pub fn from_rust(frame: AudioFrame) -> Self {
        Self { inner: frame }
    }

    pub fn as_rust(&self) -> &AudioFrame {
        &self.inner
    }

    pub fn into_rust(self) -> AudioFrame {
        self.inner
    }
}

// ========== DataFrame Wrapper ==========

#[pyclass(name = "DataFrame")]
#[derive(Clone)]
pub struct PyDataFrame {
    pub(crate) inner: DataFrame,
}

#[pymethods]
impl PyDataFrame {
    #[getter]
    fn timestamp(&self) -> f64 {
        self.inner.timestamp
    }

    #[getter]
    fn buffer(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        use super::gpu_wrappers::PyWgpuBuffer;
        let buffer_wrapper = PyWgpuBuffer {
            buffer: self.inner.buffer.clone(),
        };
        Ok(Py::new(py, buffer_wrapper)?.into_py(py))
    }

    #[getter]
    fn metadata(&self) -> HashMap<String, String> {
        self.inner
            .metadata
            .as_ref()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), format!("{:?}", v)))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn __repr__(&self) -> String {
        format!("DataFrame(timestamp={:.6}s)", self.inner.timestamp)
    }

    fn __str__(&self) -> String {
        self.__repr__()
    }
}

impl PyDataFrame {
    pub fn from_rust(frame: DataFrame) -> Self {
        Self { inner: frame }
    }

    pub fn as_rust(&self) -> &DataFrame {
        &self.inner
    }

    pub fn into_rust(self) -> DataFrame {
        self.inner
    }
}
