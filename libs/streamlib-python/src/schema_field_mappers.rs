// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Compile-time safe field mappers for frame types.
//!
//! These traits directly reference Rust struct fields - if fields are renamed
//! or removed in the core library, compilation will fail here, ensuring Python
//! bindings stay in sync with Rust types.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;
use streamlib::core::rhi::{StreamTexture, TextureFormat};
use streamlib::core::AudioChannelCount;
use streamlib::{AudioFrame, DataFrame, VideoFrame};

use crate::shader_handle::PyGpuTexture;

/// Field mapper for VideoFrame - compile-time safe field access.
pub trait VideoFrameFieldMapper {
    fn get_texture(&self) -> StreamTexture;
    fn get_format(&self) -> TextureFormat;
    fn get_timestamp_ns(&self) -> i64;
    fn get_frame_number(&self) -> u64;
    fn get_width(&self) -> u32;
    fn get_height(&self) -> u32;
}

impl VideoFrameFieldMapper for VideoFrame {
    fn get_texture(&self) -> StreamTexture {
        self.texture.clone()
    }

    fn get_format(&self) -> TextureFormat {
        self.format
    }

    fn get_timestamp_ns(&self) -> i64 {
        self.timestamp_ns
    }

    fn get_frame_number(&self) -> u64 {
        self.frame_number
    }

    fn get_width(&self) -> u32 {
        self.width
    }

    fn get_height(&self) -> u32 {
        self.height
    }
}

/// Field mapper for AudioFrame - compile-time safe field access.
pub trait AudioFrameFieldMapper {
    fn get_samples(&self) -> Arc<Vec<f32>>;
    fn get_channels(&self) -> AudioChannelCount;
    fn get_timestamp_ns(&self) -> i64;
    fn get_frame_number(&self) -> u64;
    fn get_sample_rate(&self) -> u32;
}

impl AudioFrameFieldMapper for AudioFrame {
    fn get_samples(&self) -> Arc<Vec<f32>> {
        Arc::clone(&self.samples)
    }

    fn get_channels(&self) -> AudioChannelCount {
        self.channels
    }

    fn get_timestamp_ns(&self) -> i64 {
        self.timestamp_ns
    }

    fn get_frame_number(&self) -> u64 {
        self.frame_number
    }

    fn get_sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

/// Convert a VideoFrame field to a Python object.
pub fn video_frame_field_to_py(
    py: Python<'_>,
    frame: &VideoFrame,
    field: &str,
) -> PyResult<Py<PyAny>> {
    match field {
        "texture" => Ok(PyGpuTexture::new(frame.get_texture())
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        "format" => Ok(format!("{:?}", frame.get_format())
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        "timestamp_ns" => Ok(frame
            .get_timestamp_ns()
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        "frame_number" => Ok(frame
            .get_frame_number()
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        "width" => Ok(frame.get_width().into_pyobject(py)?.into_any().unbind()),
        "height" => Ok(frame.get_height().into_pyobject(py)?.into_any().unbind()),
        _ => Err(pyo3::exceptions::PyKeyError::new_err(format!(
            "VideoFrame has no field '{}'",
            field
        ))),
    }
}

/// Convert a VideoFrame to a Python dict with all fields.
pub fn video_frame_to_dict(py: Python<'_>, frame: &VideoFrame) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("texture", PyGpuTexture::new(frame.get_texture()))?;
    dict.set_item("format", format!("{:?}", frame.get_format()))?;
    dict.set_item("timestamp_ns", frame.get_timestamp_ns())?;
    dict.set_item("frame_number", frame.get_frame_number())?;
    dict.set_item("width", frame.get_width())?;
    dict.set_item("height", frame.get_height())?;
    Ok(dict.unbind())
}

/// Convert an AudioFrame field to a Python object.
pub fn audio_frame_field_to_py(
    py: Python<'_>,
    frame: &AudioFrame,
    field: &str,
) -> PyResult<Py<PyAny>> {
    match field {
        "samples" => {
            let samples: Vec<f32> = (*frame.get_samples()).clone();
            Ok(samples.into_pyobject(py)?.into_any().unbind())
        }
        "channels" => Ok((frame.get_channels().as_usize() as u32)
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        "timestamp_ns" => Ok(frame
            .get_timestamp_ns()
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        "frame_number" => Ok(frame
            .get_frame_number()
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        "sample_rate" => Ok(frame
            .get_sample_rate()
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        _ => Err(pyo3::exceptions::PyKeyError::new_err(format!(
            "AudioFrame has no field '{}'",
            field
        ))),
    }
}

/// Convert an AudioFrame to a Python dict with all fields.
pub fn audio_frame_to_dict(py: Python<'_>, frame: &AudioFrame) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    let samples: Vec<f32> = (*frame.get_samples()).clone();
    dict.set_item("samples", samples)?;
    dict.set_item("channels", frame.get_channels().as_usize() as u32)?;
    dict.set_item("timestamp_ns", frame.get_timestamp_ns())?;
    dict.set_item("frame_number", frame.get_frame_number())?;
    dict.set_item("sample_rate", frame.get_sample_rate())?;
    Ok(dict.unbind())
}

/// Convert a DataFrame field to a Python object.
pub fn data_frame_field_to_py(
    py: Python<'_>,
    frame: &DataFrame,
    field: &str,
) -> PyResult<Py<PyAny>> {
    match field {
        "timestamp_ns" => Ok(frame.timestamp_ns.into_pyobject(py)?.into_any().unbind()),
        "data" => Ok(frame.data.clone().into_pyobject(py)?.into_any().unbind()),
        "schema_name" => Ok(frame.schema.name().into_pyobject(py)?.into_any().unbind()),
        _ => Err(pyo3::exceptions::PyKeyError::new_err(format!(
            "DataFrame has no field '{}'",
            field
        ))),
    }
}

/// Convert a DataFrame to a Python dict with all fields.
pub fn data_frame_to_dict(py: Python<'_>, frame: &DataFrame) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("timestamp_ns", frame.timestamp_ns)?;
    dict.set_item("data", frame.data.clone())?;
    dict.set_item("schema_name", frame.schema.name())?;
    Ok(dict.unbind())
}
