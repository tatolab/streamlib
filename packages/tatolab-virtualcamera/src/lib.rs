// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The native half of `tatolab.virtualcamera`: the PipeWire camera door, a
//! `Video/Source` node the engine registers in this node's helper process
//! through the module host table, fed each YUY2 picture the node converted.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use tatolab_module_sdk::{ModuleHost, ModuleHostVirtualCameraStream};

fn refused(refusal: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(refusal.to_string())
}

/// One node's PipeWire camera: a `Video/Source` node named for the camera,
/// alive until it is closed.
#[pyclass(name = "PipeWireCameraDoor", module = "tatolab.virtualcamera._native")]
struct PipeWireCameraDoor {
    camera_name: String,
    virtual_camera_stream: Option<ModuleHostVirtualCameraStream>,
}

impl PipeWireCameraDoor {
    fn open_camera(&self) -> PyResult<&ModuleHostVirtualCameraStream> {
        self.virtual_camera_stream.as_ref().ok_or_else(|| {
            refused(format!(
                "VirtualCameraSink \"{}\": the PipeWire camera is closed",
                self.camera_name
            ))
        })
    }
}

#[pymethods]
impl PipeWireCameraDoor {
    /// Register the camera through the table the node's context lent.
    #[new]
    fn new(module_host_table: &Bound<'_, PyAny>, camera_name: String) -> PyResult<Self> {
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)
            .map_err(refused)?;
        let virtual_camera_stream = module_host
            .open_virtual_camera_stream(&camera_name)
            .map_err(refused)?;
        Ok(Self {
            camera_name,
            virtual_camera_stream: Some(virtual_camera_stream),
        })
    }

    /// The camera's name in every picker.
    #[getter]
    fn camera_name(&self) -> &str {
        &self.camera_name
    }

    /// Offer one even-width extent as YUY2; consumers re-negotiate.
    #[pyo3(signature = (width, height, frames_per_second=None))]
    fn offer_extent(
        &self,
        width: u32,
        height: u32,
        frames_per_second: Option<u32>,
    ) -> PyResult<()> {
        self.open_camera()?
            .offer_extent(width, height, frames_per_second)
            .map_err(refused)
    }

    /// Copy one YUY2 picture of the offered extent from host memory at
    /// `rows_address` — `rows_byte_count` bytes, rows `stride_bytes` apart —
    /// into the camera's next free buffer, and name what became of it.
    fn write_yuyv_frame(
        &self,
        python: Python<'_>,
        rows_address: usize,
        rows_byte_count: usize,
        stride_bytes: u32,
        timestamp_ns: i64,
    ) -> PyResult<&'static str> {
        let camera = self.open_camera()?;
        if rows_address == 0 {
            return Err(refused(format!(
                "VirtualCameraSink \"{}\": a YUY2 picture is handed at a null address",
                self.camera_name
            )));
        }
        // SAFETY: the caller hands the host mapping of a pixel buffer it holds
        // locked for the length of the call, `rows_byte_count` bytes long.
        let yuyv_rows =
            unsafe { std::slice::from_raw_parts(rows_address as *const u8, rows_byte_count) };
        python
            .detach(|| camera.write_yuyv_frame(yuyv_rows, stride_bytes, timestamp_ns))
            .map(|presentation| presentation.as_str())
            .map_err(refused)
    }

    /// Why PipeWire dropped the camera node, or `None` while it lives.
    #[getter]
    fn failure_that_ended_the_camera(&self) -> PyResult<Option<String>> {
        self.open_camera()?
            .failure_that_ended_the_stream()
            .map_err(refused)
    }

    /// Remove the camera node; every later call is refused.
    fn close(&mut self) {
        self.virtual_camera_stream = None;
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PipeWireCameraDoor>()?;
    Ok(())
}
