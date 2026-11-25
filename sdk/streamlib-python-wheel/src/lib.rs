// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
mod python_message_kind_read_types;
#[cfg(target_os = "macos")]
mod python_metal_framework_queue_synchronization;
mod python_module_host_table;
mod python_monotonic_timer;
mod python_node_context;
mod python_node_declaration;
mod python_node_link_data_access;
mod python_port_payload;
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
    module.add_class::<python_node_link_data_access::PythonNodeLinkDataAccess>()?;
    module.add_class::<python_port_payload::PythonPortPayloadBorrow>()?;
    module.add_class::<python_port_payload::PythonPortPayloadLoan>()?;
    module.add_class::<python_message_kind_read_types::PythonAudio>()?;
    module.add_class::<python_message_kind_read_types::PythonEncodedMedia>()?;
    module.add_class::<python_message_kind_read_types::PythonTensorCpuAttribute>()?;
    module.add_function(wrap_pyfunction!(
        python_node_link_data_access::open_node_link_data_access_for_helper_process,
        module
    )?)?;
    module.add_class::<python_node_context::PythonRuntimeContextFullAccess>()?;
    module.add_function(wrap_pyfunction!(
        python_node_context::open_runtime_context_full_access_for_helper_process,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_node_context::limited_access_view_of_runtime_context_for_helper_process,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_node_context::note_pause_state_from_parent_on_runtime_context,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_node_context::note_stop_request_from_parent_on_runtime_context,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_node_context::note_execution_mode_from_parent_on_runtime_context,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_node_context::not_ready_report_on_runtime_context,
        module
    )?)?;
    module.add_class::<python_node_context::PythonRuntimeContextLimitedAccess>()?;
    module.add_class::<python_node_context::PythonGpuContextFullAccess>()?;
    module.add_class::<python_node_context::PythonGpuContextLimitedAccess>()?;
    module.add_class::<python_node_context::PythonGpuSurfaceHandle>()?;
    module.add_class::<python_node_context::PythonGpuSurfaceDeviceTensorScope>()?;
    module.add_class::<python_node_context::PythonGpuSurfaceCheckOutLease>()?;
    module.add_class::<python_node_context::PythonOpaqueFdAllocationExport>()?;
    module.add_class::<python_node_context::PythonIOSurfaceMachPortExport>()?;
    module.add_class::<python_node_context::PythonComputeKernel>()?;
    module.add_class::<python_node_context::PythonGraphicsKernel>()?;
    module.add_class::<python_node_context::PythonRayTracingKernel>()?;
    module.add_class::<python_node_context::PythonAccelerationStructureHandle>()?;
    module.add_class::<python_node_context::PythonKernelDispatchBatch>()?;
    module.add_class::<python_node_context::PythonLinkInputDataReader>()?;
    module.add_class::<python_node_context::PythonLinkOutputDataWriter>()?;
    module.add_class::<python_node_context::PythonStoreScopedToThisNode>()?;
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
        python_function_node_island::why_each_function_node_its_islands_call_is_raising,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::take_each_function_node_raise_newly_noted,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::read_the_message_at_an_input_port_unread,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::read_the_control_value_at_an_input_port,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::read_the_parts_at_an_input_port,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::read_the_stored_value_at_an_input_port,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::write_the_stored_value_on_an_output_port,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_node_context::read_the_bundle_frame_at_an_input_port,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_node_context::write_the_bundle_frame_on_an_output_port,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::read_the_named_value_at_an_input_port,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::write_the_named_value_on_an_output_port,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::hand_an_unread_message_on,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_function_node_island::an_unread_message_carries_what_another_does,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_input_port_pin_values::read_each_unlinked_input_port_as_its_pin_value,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_input_port_pin_values::queue_a_fire_of_each_trigger_input,
        module
    )?)?;
    module.add_class::<python_function_node_island::FunctionNodeEvaluatedAsAStandaloneNode>()?;
    module.add_class::<python_function_node_island::UnreadMessageAFunctionNodeHandsOn>()?;
    module.add_function(wrap_pyfunction!(
        python_bag_conversion::gpu_limited_access_of_the_typed_read_in_progress,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_node_context::engine_build_id_compiled_into_this_extension,
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
