// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of the streamlib wheel — the extension module CPython
//! imports as `tatolab.runtime._engine`.

use pyo3::prelude::*;

#[cfg(target_os = "macos")]
mod darwin_close_on_exec_kqueue;
mod python_bag_conversion;
#[cfg(test)]
mod python_class_from_source_for_tests;
mod python_control_plane_hosting;
#[cfg(target_os = "linux")]
mod python_cuda_pixel_exchange;
mod python_gpu_surface_pixel_exchange;
#[cfg(target_os = "macos")]
mod python_helper_process_parent_death_watch;
mod python_helper_process_pixel_exchange;
mod python_local_api_mcp_client;
mod python_logging;
#[cfg(target_os = "macos")]
mod python_metal_framework_queue_synchronization;
mod python_monotonic_timer;
mod python_native_builtin_blocks;
mod python_processor_context;
mod python_processor_declaration;
mod python_processor_link_data_access;
mod python_processor_owned_window;
mod python_runtime_lifecycle;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod python_surface_share_service_for_tests;
mod python_test_harness_endpoints;

pub use python_runtime_lifecycle::PythonRuntimeHandle;

#[pymodule]
fn _engine(module: &Bound<'_, PyModule>) -> PyResult<()> {
    python_native_builtin_blocks::register_native_builtin_processor_types();
    python_test_harness_endpoints::register_test_harness_processor_types();
    module.add_class::<PythonRuntimeHandle>()?;
    python_test_harness_endpoints::add_test_harness_marker_classes_to_the_module(module)?;
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
    python_local_api_mcp_client::register_local_api_mcp_client(module)?;
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
        python_runtime_lifecycle::processor_class_import_paths_in_this_processes_catalog,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_processor_context::engine_build_id_compiled_into_this_extension,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(python_logging::monotonic_now_ns, module)?)?;
    module.add_function(wrap_pyfunction!(python_logging::log_event, module)?)?;
    module.add_function(wrap_pyfunction!(
        python_logging::capture_this_helper_processes_engine_log_records,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_logging::drain_the_engine_log_records_this_helper_captured,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_logging::runtime_log_directory,
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
    module.add_function(wrap_pyfunction!(
        python_test_harness_endpoints::open_test_harness_channel,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_test_harness_endpoints::close_test_harness_channel,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_test_harness_endpoints::feed_test_harness_bag,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_test_harness_endpoints::await_test_harness_bag,
        module
    )?)?;
    Ok(())
}
