// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `tatolab.runtime` — the extension module a processor
//! interpreter imports as `tatolab.runtime._engine`.

use pyo3::prelude::*;

#[cfg(target_os = "macos")]
mod darwin_close_on_exec_kqueue;
mod python_bag_conversion;
#[cfg(test)]
mod python_class_from_source_for_tests;
#[cfg(target_os = "linux")]
mod python_cuda_pixel_exchange;
mod python_gpu_surface_pixel_exchange;
#[cfg(target_os = "macos")]
mod python_helper_process_parent_death_watch;
mod python_helper_process_pixel_exchange;
mod python_logging;
#[cfg(target_os = "macos")]
mod python_metal_framework_queue_synchronization;
mod python_monotonic_timer;
mod python_processor_context;
mod python_processor_declaration;
mod python_processor_link_data_access;
mod python_processor_owned_window;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod python_surface_share_service_for_tests;

#[pymodule]
fn _engine(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<python_processor_link_data_access::PythonProcessorLinkDataAccess>()?;
    module.add_function(wrap_pyfunction!(
        python_processor_link_data_access::open_node_link_data_access_for_helper_process,
        module
    )?)?;
    module.add_class::<python_processor_context::PythonRuntimeContextFullAccess>()?;
    module.add_function(wrap_pyfunction!(
        python_processor_context::open_runtime_context_full_access_for_helper_process,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_processor_context::limited_access_view_of_runtime_context_for_helper_process,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_processor_context::note_pause_state_from_parent_on_runtime_context,
        module
    )?)?;
    module.add_class::<python_processor_context::PythonRuntimeContextLimitedAccess>()?;
    module.add_class::<python_processor_context::PythonGpuContextFullAccess>()?;
    module.add_class::<python_processor_context::PythonGpuContextLimitedAccess>()?;
    module.add_class::<python_processor_context::PythonGpuSurfaceHandle>()?;
    module.add_class::<python_processor_context::PythonGpuSurfaceDeviceTensorScope>()?;
    module.add_class::<python_processor_context::PythonGpuSurfaceCheckOutLease>()?;
    module.add_class::<python_processor_context::PythonOpaqueFdTextureExport>()?;
    module.add_class::<python_processor_context::PythonIOSurfaceMachPortExport>()?;
    module.add_class::<python_processor_context::PythonComputeKernel>()?;
    module.add_class::<python_processor_context::PythonGraphicsKernel>()?;
    module.add_class::<python_processor_context::PythonRayTracingKernel>()?;
    module.add_class::<python_processor_context::PythonAccelerationStructureHandle>()?;
    module.add_class::<python_processor_context::PythonKernelDispatchBatch>()?;
    module.add_class::<python_processor_owned_window::PythonProcessorOwnedWindow>()?;
    module.add_class::<python_processor_owned_window::PythonProcessorOwnedWindowEvents>()?;
    module.add_class::<python_processor_context::PythonLinkInputDataReader>()?;
    module.add_class::<python_processor_context::PythonLinkOutputDataWriter>()?;
    module.add_class::<python_monotonic_timer::PythonMonotonicTimer>()?;
    module.add_function(wrap_pyfunction!(
        python_bag_conversion::gpu_limited_access_of_the_typed_read_in_progress,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_bag_conversion::decode_tapped_channel_bag_frame_to_python_object,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_bag_conversion::encode_bag_to_msgpack_bytes,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_bag_conversion::decode_msgpack_bytes_to_python_object,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_processor_context::engine_build_id_compiled_into_this_extension,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(python_logging::monotonic_now_ns, module)?)?;
    module.add_function(wrap_pyfunction!(
        python_logging::capture_this_helper_processes_engine_log_records,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_logging::drain_the_engine_log_records_this_helper_captured,
        module
    )?)?;
    #[cfg(target_os = "macos")]
    {
        module.add_function(wrap_pyfunction!(
            python_helper_process_parent_death_watch::watch_for_this_helper_processes_parent_going_away,
            module
        )?)?;
        module.add_function(wrap_pyfunction!(
            python_helper_process_parent_death_watch::note_this_helper_processes_callbacks_returned_after_its_parent_went_away,
            module
        )?)?;
    }
    Ok(())
}
