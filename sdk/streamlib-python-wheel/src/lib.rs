// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of the streamlib wheel — the extension module CPython
//! imports as `streamlib._engine`.

use pyo3::prelude::*;

mod python_added_processor;
mod python_bag_conversion;
mod python_control_plane_hosting;
#[cfg(target_os = "linux")]
mod python_cuda_pixel_exchange;
mod python_gpu_surface_pixel_exchange;
mod python_helper_process_spawn_host;
mod python_logging;
mod python_monotonic_timer;
mod python_native_builtin_blocks;
mod python_processor_context;
mod python_processor_declaration;
mod python_processor_import_path;
mod python_processor_link_data_access;
mod python_processor_registration;
mod python_runtime_lifecycle;

pub use python_runtime_lifecycle::PythonRuntimeHandle;

#[pymodule]
fn _engine(module: &Bound<'_, PyModule>) -> PyResult<()> {
    python_native_builtin_blocks::register_native_builtin_processor_types();
    module.add_class::<PythonRuntimeHandle>()?;
    module.add_class::<python_native_builtin_blocks::PythonTestPatternSourceBlock>()?;
    module.add_class::<python_native_builtin_blocks::PythonCameraSourceBlock>()?;
    module.add_class::<python_native_builtin_blocks::PythonDisplayWindowBlock>()?;
    module.add_class::<python_added_processor::PythonAddedProcessor>()?;
    module.add_class::<python_added_processor::PythonProcessorOutputPortReference>()?;
    module.add_class::<python_added_processor::PythonProcessorInputPortReference>()?;
    module.add_class::<python_processor_link_data_access::PythonProcessorLinkDataAccess>()?;
    module.add_class::<python_processor_context::PythonRuntimeContextFullAccess>()?;
    module.add_class::<python_processor_context::PythonRuntimeContextLimitedAccess>()?;
    module.add_class::<python_processor_context::PythonGpuContextFullAccess>()?;
    module.add_class::<python_processor_context::PythonGpuContextLimitedAccess>()?;
    module.add_class::<python_processor_context::PythonGpuSurfaceHandle>()?;
    module.add_class::<python_processor_context::PythonLinkInputDataReader>()?;
    module.add_class::<python_processor_context::PythonLinkOutputDataWriter>()?;
    module.add_class::<python_monotonic_timer::PythonMonotonicTimer>()?;
    module.add_function(wrap_pyfunction!(
        python_logging::media_clock_now_ns,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(python_logging::monotonic_now_ns, module)?)?;
    module.add_function(wrap_pyfunction!(python_logging::log_event, module)?)?;
    Ok(())
}
