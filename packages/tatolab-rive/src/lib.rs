// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `tatolab-rive`, imported as `tatolab.rive._native`.
//!
//! The wheel's `@node.process` hands this module its module host table, and a thread
//! here draws every frame with rive-runtime straight into the node's output
//! texture and publishes it, with no Python attached.

#[cfg(not(target_os = "linux"))]
compile_error!("tatolab-rive draws on the consumer RHI's external-renderer device, Linux only");

pub mod rive_image_frame;
pub mod rive_scene_bridge;
pub mod rive_source_frame_loop;
pub mod view_model_data_message;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use tatolab_module_sdk::ModuleHost;

use crate::rive_source_frame_loop::{RiveSourceRendering, RiveSourceSettings};

/// A `.riv` scene publishing Image frames on a lent output port from a thread
/// this wheel owns.
#[pyclass(name = "RiveSceneRendering")]
struct PythonRiveSceneRendering {
    rendering: RiveSourceRendering,
}

#[pymethods]
impl PythonRiveSceneRendering {
    #[new]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        module_host_table,
        video_output_port,
        data_input_port,
        riv_path,
        artboard,
        state_machine,
        width,
        height,
        frame_rate,
        output_pool_rotation_depth,
    ))]
    fn new(
        python: Python<'_>,
        module_host_table: &Bound<'_, PyAny>,
        video_output_port: String,
        data_input_port: String,
        riv_path: String,
        artboard: Option<String>,
        state_machine: Option<String>,
        width: u32,
        height: u32,
        frame_rate: u32,
        output_pool_rotation_depth: u32,
    ) -> PyResult<Self> {
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        let settings = RiveSourceSettings {
            riv_path,
            artboard,
            state_machine,
            width,
            height,
            frame_rate,
            output_pool_rotation_depth,
        };
        let rendering = python
            .detach(|| {
                RiveSourceRendering::start(
                    module_host,
                    &video_output_port,
                    &data_input_port,
                    settings,
                )
            })
            .map_err(PyRuntimeError::new_err)?;
        Ok(Self { rendering })
    }

    /// Stop rendering and release the device. Idempotent.
    fn stop(&mut self, python: Python<'_>) {
        python.detach(|| self.rendering.stop());
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PythonRiveSceneRendering>()?;
    Ok(())
}
