// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `tatolab-camera-proof`, imported as `tatolab.camera_proof._native`.
//!
//! The engine never calls anything here: the wheel's `@node` reaches it from
//! its own Python, in its own processor interpreter.

#[cfg(not(target_os = "linux"))]
compile_error!("tatolab-camera-proof captures through V4L2, which exists only on Linux");

pub mod capture_timestamp;
pub mod v4l2_camera_capture;
pub mod v4l2_colour_description;

use std::time::Duration;

use pyo3::exceptions::{PyOSError, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::capture_timestamp::ResolvedCaptureTimestamp;
use crate::v4l2_camera_capture::{CameraCaptureRefusal, V4l2CameraCapture};
use crate::v4l2_colour_description::luma_weights_for_matrix;

/// A camera streaming on a thread this wheel owns; reading takes the newest frame.
#[pyclass(name = "V4l2CameraCapture")]
struct PythonV4l2CameraCapture {
    capture: V4l2CameraCapture,
}

#[pymethods]
impl PythonV4l2CameraCapture {
    #[new]
    #[pyo3(signature = (device_path, max_width, max_height))]
    fn new(
        python: Python<'_>,
        device_path: Option<String>,
        max_width: u32,
        max_height: u32,
    ) -> PyResult<Self> {
        let capture = python
            .detach(|| V4l2CameraCapture::open(device_path.as_deref(), max_width, max_height))
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
    fn rows_in_one_frame(&self) -> u32 {
        self.capture.format().rows_in_one_frame()
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

    /// `(Kr, Kb)` for the matrix the frames were encoded with.
    fn luma_weights(&self) -> (f32, f32) {
        let format = self.capture.format();
        luma_weights_for_matrix(format.colour.matrix, format.height)
    }

    /// The newest frame not yet read, or `None` after `timeout_ms` with none.
    fn wait_for_newest_frame(
        &self,
        python: Python<'_>,
        timeout_ms: u64,
    ) -> PyResult<Option<CapturedCameraFrame>> {
        let mut frame_bytes = Vec::new();
        let readout = python
            .detach(|| {
                self.capture
                    .wait_for_newest_frame(Duration::from_millis(timeout_ms), &mut frame_bytes)
            })
            .map_err(capture_ended)?;
        let Some(readout) = readout else {
            return Ok(None);
        };
        let timestamp_source = match readout.timestamp {
            ResolvedCaptureTimestamp::FromTheDevice(_) => "device",
            ResolvedCaptureTimestamp::ClampedToDequeue(_) => "clamped_to_dequeue",
            ResolvedCaptureTimestamp::DequeueInstant(_) => "dequeue",
        };
        Ok(Some(CapturedCameraFrame {
            frame_bytes: PyBytes::new(python, &frame_bytes).unbind(),
            timestamp_ns: readout.timestamp.timestamp_ns(),
            timestamp_source,
            driver_sequence: readout.driver_sequence,
            frames_captured: readout.frames_captured,
            frames_replaced_before_they_were_read: readout.frames_replaced_before_they_were_read,
        }))
    }

    /// Stop streaming and release the device. Idempotent.
    fn stop(&mut self, python: Python<'_>) {
        python.detach(|| self.capture.stop());
    }
}

/// One captured frame: its rows, and when it was taken.
#[pyclass(frozen, get_all)]
struct CapturedCameraFrame {
    frame_bytes: Py<PyBytes>,
    timestamp_ns: i64,
    timestamp_source: &'static str,
    driver_sequence: u32,
    frames_captured: u64,
    frames_replaced_before_they_were_read: u64,
}

fn capture_ended(refusal: CameraCaptureRefusal) -> PyErr {
    PyRuntimeError::new_err(format!("camera capture ended: {refusal}"))
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
    module.add_class::<PythonV4l2CameraCapture>()?;
    module.add_class::<CapturedCameraFrame>()?;
    module.add_function(wrap_pyfunction!(list_v4l2_capture_devices, module)?)?;
    module.add_function(wrap_pyfunction!(monotonic_now_ns, module)?)?;
    Ok(())
}
