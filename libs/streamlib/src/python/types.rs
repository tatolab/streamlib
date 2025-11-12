
use pyo3::prelude::*;
use crate::core::{VideoFrame, AudioFrame, DataFrame};
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
        self.inner.timestamp
    }

    #[getter]
    fn frame_number(&self) -> u64 {
        self.inner.frame_number
    }

    #[getter]
    fn data(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        use super::gpu_wrappers::PyWgpuTexture;
        let texture_wrapper = PyWgpuTexture {
            texture: self.inner.texture.clone()
        };
        Ok(Py::new(py, texture_wrapper)?.into_py(py))
    }

    fn clone_with_texture(&self, py: Python<'_>, new_texture: &Bound<'_, PyAny>) -> PyResult<PyVideoFrame> {
        use super::gpu_wrappers::PyWgpuTexture;

        let texture_wrapper: Py<PyWgpuTexture> = new_texture.extract()?;
        let texture_arc = texture_wrapper.borrow(py).texture.clone();

        let new_frame = VideoFrame {
            texture: texture_arc,
            format: self.inner.format,
            width: self.inner.width,
            height: self.inner.height,
            timestamp: self.inner.timestamp,
            frame_number: self.inner.frame_number,
            metadata: self.inner.metadata.clone(),
        };

        Ok(PyVideoFrame { inner: new_frame })
    }

    #[getter]
    fn metadata(&self) -> HashMap<String, String> {
        self.inner.metadata.as_ref()
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

// ========== AudioFrame Wrappers ==========

macro_rules! impl_py_audio_frame {
    ($name:ident, $py_name:literal, $channels:expr) => {
        #[pyclass(name = $py_name)]
        #[derive(Clone)]
        pub struct $name {
            pub(crate) inner: AudioFrame<$channels>,
        }

        #[pymethods]
        impl $name {
            #[getter]
            fn channels(&self) -> usize {
                $channels
            }

            #[getter]
            fn sample_count(&self) -> usize {
                self.inner.sample_count()
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
                if channel >= $channels {
                    return Err(pyo3::exceptions::PyValueError::new_err(
                        format!("Channel {} out of range (0-{})", channel, $channels - 1)
                    ));
                }

                let sample_count = self.inner.sample_count();
                let mut channel_samples = Vec::with_capacity(sample_count);

                for i in 0..sample_count {
                    channel_samples.push(self.inner.samples[i * $channels + channel]);
                }

                Ok(channel_samples)
            }

            #[getter]
            fn metadata(&self) -> HashMap<String, String> {
                self.inner.metadata.as_ref()
                    .map(|m| {
                        m.iter()
                            .map(|(k, v)| (k.clone(), format!("{:?}", v)))
                            .collect()
                    })
                    .unwrap_or_default()
            }

            fn __repr__(&self) -> String {
                format!(
                    "AudioFrame<{}>({} samples, frame={})",
                    $channels,
                    self.inner.sample_count(),
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

        impl $name {
            pub fn from_rust(frame: AudioFrame<$channels>) -> Self {
                Self { inner: frame }
            }

            pub fn as_rust(&self) -> &AudioFrame<$channels> {
                &self.inner
            }

            pub fn into_rust(self) -> AudioFrame<$channels> {
                self.inner
            }
        }
    };
}

// Generate wrappers for all supported channel counts
impl_py_audio_frame!(PyAudioFrame1, "AudioFrame1", 1);
impl_py_audio_frame!(PyAudioFrame2, "AudioFrame2", 2);
impl_py_audio_frame!(PyAudioFrame4, "AudioFrame4", 4);
impl_py_audio_frame!(PyAudioFrame6, "AudioFrame6", 6);
impl_py_audio_frame!(PyAudioFrame8, "AudioFrame8", 8);

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
            buffer: self.inner.buffer.clone()
        };
        Ok(Py::new(py, buffer_wrapper)?.into_py(py))
    }

    #[getter]
    fn metadata(&self) -> HashMap<String, String> {
        self.inner.metadata.as_ref()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), format!("{:?}", v)))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn __repr__(&self) -> String {
        format!(
            "DataFrame(timestamp={:.6}s)",
            self.inner.timestamp
        )
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
