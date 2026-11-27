// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The native half of `tatolab-rive`, imported as `tatolab.rive._native`.
//!
//! The node's body hands this module its module host table, and each scene a
//! build opens draws every frame on a thread here with rive-runtime straight into
//! the node's output texture and publishes it, with no Python attached.

#[cfg(not(target_os = "linux"))]
compile_error!("tatolab-rive draws on the consumer RHI's external-renderer device, Linux only");

pub mod rive_image_frame;
pub mod rive_scene_bridge;
pub mod rive_source_frame_loop;
pub mod view_model_data_message;

use pyo3::create_exception;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use tatolab_module_sdk::ModuleHost;

use crate::rive_source_frame_loop::{RiveSceneRefused, RiveSourceRendering, RiveSourceSettings};

create_exception!(
    tatolab.rive._native,
    RiveSceneNotBuilt,
    PyRuntimeError,
    "A scene could not be built; each reason is its own subclass."
);
create_exception!(
    tatolab.rive._native,
    RiveSizeNotPositive,
    RiveSceneNotBuilt,
    "`width`, `height` or `frame_rate` is 0."
);
create_exception!(
    tatolab.rive._native,
    RiveFileUnreadable,
    RiveSceneNotBuilt,
    "The `.riv` file could not be read."
);
create_exception!(
    tatolab.rive._native,
    RiveFileRefused,
    RiveSceneNotBuilt,
    "rive-runtime refused the file, or it holds no artboard or state machine of the name given."
);
create_exception!(
    tatolab.rive._native,
    RivePortRefused,
    RiveSceneNotBuilt,
    "The `video` or `data` port the node lent could not be opened."
);
create_exception!(
    tatolab.rive._native,
    RiveDeviceRefused,
    RiveSceneNotBuilt,
    "No external-renderer device could be created on the runtime's GPU."
);
create_exception!(
    tatolab.rive._native,
    RiveSurfaceShareUnreachable,
    RiveSceneNotBuilt,
    "The surface share that output textures are checked out through is unreachable."
);
create_exception!(
    tatolab.rive._native,
    RiveFrameThreadFailed,
    RiveSceneNotBuilt,
    "The frame thread could not start, or ended before its scene was built."
);

impl From<RiveSceneRefused> for PyErr {
    fn from(refusal: RiveSceneRefused) -> Self {
        let why = refusal.to_string();
        match refusal {
            RiveSceneRefused::SizeNotPositive(_) => RiveSizeNotPositive::new_err(why),
            RiveSceneRefused::FileUnreadable(_) => RiveFileUnreadable::new_err(why),
            RiveSceneRefused::SceneRefusedByRive(_) => RiveFileRefused::new_err(why),
            RiveSceneRefused::PortRefused(_) => RivePortRefused::new_err(why),
            RiveSceneRefused::DeviceRefused(_) => RiveDeviceRefused::new_err(why),
            RiveSceneRefused::SurfaceShareUnreachable(_) => {
                RiveSurfaceShareUnreachable::new_err(why)
            }
            RiveSceneRefused::FrameThreadFailed(_) => RiveFrameThreadFailed::new_err(why),
        }
    }
}

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
        let rendering = python.detach(|| {
            RiveSourceRendering::start(module_host, &video_output_port, &data_input_port, settings)
        })?;
        Ok(Self { rendering })
    }

    /// Stop rendering and release the device. Idempotent.
    fn close(&mut self, python: Python<'_>) {
        python.detach(|| self.rendering.close());
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PythonRiveSceneRendering>()?;
    let python = module.py();
    module.add("RiveSceneNotBuilt", python.get_type::<RiveSceneNotBuilt>())?;
    module.add(
        "RiveSizeNotPositive",
        python.get_type::<RiveSizeNotPositive>(),
    )?;
    module.add(
        "RiveFileUnreadable",
        python.get_type::<RiveFileUnreadable>(),
    )?;
    module.add("RiveFileRefused", python.get_type::<RiveFileRefused>())?;
    module.add("RivePortRefused", python.get_type::<RivePortRefused>())?;
    module.add("RiveDeviceRefused", python.get_type::<RiveDeviceRefused>())?;
    module.add(
        "RiveSurfaceShareUnreachable",
        python.get_type::<RiveSurfaceShareUnreachable>(),
    )?;
    module.add(
        "RiveFrameThreadFailed",
        python.get_type::<RiveFrameThreadFailed>(),
    )?;
    Ok(())
}
