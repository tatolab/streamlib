// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `tatolab.runtime` — the extension module a processor
//! interpreter imports as `tatolab.runtime._engine`.

use pyo3::prelude::*;

#[cfg(target_os = "macos")]
mod darwin_close_on_exec_kqueue;
#[cfg(unix)]
mod helper_process_native_escalate_channel;
mod python_bag_conversion;
#[cfg(test)]
mod python_class_from_source_for_tests;
#[cfg(target_os = "linux")]
mod python_cuda_pixel_exchange;
mod python_function_node_island;
mod python_gpu_surface_pixel_exchange;
#[cfg(target_os = "macos")]
mod python_helper_process_parent_death_watch;
mod python_helper_process_pixel_exchange;
#[cfg(unix)]
mod python_helper_scheduling_loop;
mod python_helper_stream_clock;
mod python_input_port_pin_values;
mod python_logging;
#[cfg(target_os = "macos")]
mod python_metal_framework_queue_synchronization;
mod python_module_host_table;
mod python_monotonic_timer;
mod python_port_family_read_types;
mod python_port_payload;
mod python_processor_context;
mod python_processor_declaration;
mod python_processor_link_data_access;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod python_surface_share_service_for_tests;

#[pymodule]
fn _engine(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add(
        "MODULE_HOST_TABLE_ABI_MAJOR",
        tatolab_module_sdk::MODULE_HOST_TABLE_ABI_MAJOR,
    )?;
    module.add(
        "MODULE_HOST_TABLE_ABI_MINOR",
        tatolab_module_sdk::MODULE_HOST_TABLE_ABI_MINOR,
    )?;
    module.add_class::<python_processor_link_data_access::PythonProcessorLinkDataAccess>()?;
    module.add_class::<python_port_payload::PythonPortPayloadBorrow>()?;
    module.add_class::<python_port_payload::PythonPortPayloadLoan>()?;
    module.add_class::<python_port_family_read_types::PythonChannelsBlock>()?;
    module.add_class::<python_port_family_read_types::PythonPacketsUnit>()?;
    module.add_class::<python_port_family_read_types::PythonArrayFrameCpuAttribute>()?;
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
    module.add_function(wrap_pyfunction!(
        python_processor_context::note_stop_request_from_parent_on_runtime_context,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_processor_context::note_execution_mode_from_parent_on_runtime_context,
        module
    )?)?;
    module.add_class::<python_processor_context::PythonRuntimeContextLimitedAccess>()?;
    module.add_class::<python_processor_context::PythonGpuContextFullAccess>()?;
    module.add_class::<python_processor_context::PythonGpuContextLimitedAccess>()?;
    module.add_class::<python_processor_context::PythonGpuSurfaceHandle>()?;
    module.add_class::<python_processor_context::PythonGpuSurfaceDeviceTensorScope>()?;
    module.add_class::<python_processor_context::PythonGpuSurfaceCheckOutLease>()?;
    module.add_class::<python_processor_context::PythonOpaqueFdAllocationExport>()?;
    module.add_class::<python_processor_context::PythonIOSurfaceMachPortExport>()?;
    module.add_class::<python_processor_context::PythonComputeKernel>()?;
    module.add_class::<python_processor_context::PythonGraphicsKernel>()?;
    module.add_class::<python_processor_context::PythonRayTracingKernel>()?;
    module.add_class::<python_processor_context::PythonAccelerationStructureHandle>()?;
    module.add_class::<python_processor_context::PythonKernelDispatchBatch>()?;
    module.add_class::<python_processor_context::PythonLinkInputDataReader>()?;
    module.add_class::<python_processor_context::PythonLinkOutputDataWriter>()?;
    module.add_class::<python_monotonic_timer::PythonMonotonicTimer>()?;
    module.add_class::<python_helper_stream_clock::PythonStreamClockReading>()?;
    #[cfg(unix)]
    module.add_function(wrap_pyfunction!(
        python_helper_scheduling_loop::run_the_scheduling_loop_of_this_helper,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::feed_an_input_port_from_a_function_node_island,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::stop_feeding_an_input_port_from_a_function_node_island,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::read_the_message_at_an_input_port_unread,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::hand_an_unread_message_on,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_input_port_pin_values::read_each_unlinked_input_port_as_its_pin_value,
        module
    )?)?;
    module
        .add_class::<python_function_node_island::FunctionNodeEvaluatedAsItsHelpersProcessor>()?;
    module.add_class::<python_function_node_island::UnreadMessageAFunctionNodeHandsOn>()?;
    module.add_function(wrap_pyfunction!(
        python_bag_conversion::gpu_limited_access_of_the_typed_read_in_progress,
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
