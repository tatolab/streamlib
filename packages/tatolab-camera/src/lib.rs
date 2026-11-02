// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `tatolab-camera`, imported as `tatolab.camera._native`.
//!
//! The engine never calls anything here: the wheel's `@node` hands this module
//! its module host table, and a thread here captures, converts and publishes
//! every frame through it with no Python attached.

#[cfg(not(target_os = "linux"))]
compile_error!("tatolab-camera captures through V4L2 on Linux; its macOS half is not built yet");

pub mod capture_timestamp;
pub mod v4l2_camera_capture;
pub mod v4l2_colour_description;
pub mod video_frame_record;

use pyo3::exceptions::PyOSError;
use pyo3::prelude::*;
use tatolab_module_sdk::ModuleHost;

use crate::v4l2_camera_capture::V4l2ZeroCopyCameraCapture;

/// A camera publishing `VideoFrame`s on a lent output port from a thread this
/// wheel owns.
#[pyclass(name = "V4l2ZeroCopyCameraCapture")]
struct PythonV4l2ZeroCopyCameraCapture {
    capture: V4l2ZeroCopyCameraCapture,
}

#[pymethods]
impl PythonV4l2ZeroCopyCameraCapture {
    #[new]
    #[pyo3(signature = (
        module_host_table,
        video_output_port,
        device_path,
        max_width,
        max_height,
        output_pool_rotation_depth,
    ))]
    fn new(
        python: Python<'_>,
        module_host_table: &Bound<'_, PyAny>,
        video_output_port: String,
        device_path: Option<String>,
        max_width: u32,
        max_height: u32,
        output_pool_rotation_depth: u32,
    ) -> PyResult<Self> {
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        let capture = python
            .detach(|| {
                V4l2ZeroCopyCameraCapture::start(
                    module_host,
                    &video_output_port,
                    device_path.as_deref(),
                    max_width,
                    max_height,
                    output_pool_rotation_depth,
                )
            })
            .map_err(|refusal| PyOSError::new_err(refusal.0))?;
        Ok(Self { capture })
    }

    #[getter]
    fn device_path(&self) -> String {
        self.capture.format().device_path.clone()
    }

    #[getter]
    fn card_name(&self) -> String {
        self.capture.format().card_name.clone()
    }

    #[getter]
    fn fourcc(&self) -> &'static str {
        self.capture.format().pixel_layout.fourcc_name()
    }

    #[getter]
    fn width(&self) -> u32 {
        self.capture.format().width
    }

    #[getter]
    fn height(&self) -> u32 {
        self.capture.format().height
    }

    #[getter]
    fn bytes_per_line(&self) -> u32 {
        self.capture.format().bytes_per_line
    }

    #[getter]
    fn nominal_frames_per_second(&self) -> Option<u32> {
        self.capture.format().nominal_frames_per_second
    }

    #[getter]
    fn colour_primaries(&self) -> Option<&'static str> {
        self.capture.format().colour.primaries
    }

    #[getter]
    fn colour_transfer(&self) -> Option<&'static str> {
        self.capture.format().colour.transfer
    }

    #[getter]
    fn colour_matrix(&self) -> Option<&'static str> {
        self.capture.format().colour.matrix
    }

    #[getter]
    fn colour_range(&self) -> Option<&'static str> {
        self.capture.format().colour.range
    }

    /// Stop streaming and release the device. Idempotent.
    fn stop(&mut self, python: Python<'_>) {
        python.detach(|| self.capture.stop());
    }
}

/// Every `/dev/video*` node that reports video capture, as `(path, card name)`.
#[pyfunction]
fn list_v4l2_capture_devices(python: Python<'_>) -> Vec<(String, String)> {
    python.detach(v4l2_camera_capture::list_v4l2_capture_devices)
}

/// Nanoseconds on the clock capture timestamps are on.
#[pyfunction]
fn monotonic_now_ns() -> i64 {
    capture_timestamp::monotonic_now_ns()
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PythonV4l2ZeroCopyCameraCapture>()?;
    module.add_function(wrap_pyfunction!(list_v4l2_capture_devices, module)?)?;
    module.add_function(wrap_pyfunction!(monotonic_now_ns, module)?)?;
    Ok(())
}
