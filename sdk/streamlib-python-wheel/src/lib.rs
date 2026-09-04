// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of the streamlib wheel — the extension module CPython
//! imports as `streamlib._engine`.

use pyo3::prelude::*;

mod python_added_processor;
mod python_bag_conversion;
mod python_capability_extension_host;
#[cfg(test)]
mod python_class_from_source_for_tests;
mod python_control_plane_hosting;
#[cfg(target_os = "linux")]
mod python_cuda_pixel_exchange;
mod python_gpu_surface_pixel_exchange;
mod python_helper_process_pixel_exchange;
mod python_helper_process_spawn_host;
mod python_logging;
mod python_monotonic_timer;
mod python_native_builtin_blocks;
mod python_processor_context;
mod python_processor_declaration;
mod python_processor_import_path;
mod python_processor_link_data_access;
mod python_processor_owned_window;
mod python_processor_registration;
mod python_runtime_lifecycle;
#[cfg(all(test, target_os = "linux"))]
mod python_surface_share_service_for_tests;
mod python_test_harness_endpoints;

pub use python_runtime_lifecycle::PythonRuntimeHandle;

#[pymodule]
fn _engine(module: &Bound<'_, PyModule>) -> PyResult<()> {
    python_native_builtin_blocks::register_native_builtin_processor_types();
    python_test_harness_endpoints::register_test_harness_processor_types();
    module.add_class::<PythonRuntimeHandle>()?;
    module.add_class::<python_capability_extension_host::PythonCapabilityExtensionHost>()?;
    module.add_class::<python_native_builtin_blocks::PythonTestPatternSourceBlock>()?;
    module.add_class::<python_native_builtin_blocks::PythonCameraSourceBlock>()?;
    module.add_class::<python_native_builtin_blocks::PythonDisplayWindowBlock>()?;
    module.add_class::<python_native_builtin_blocks::PythonMicrophoneSourceBlock>()?;
    module.add_class::<python_native_builtin_blocks::PythonSpeakerSinkBlock>()?;
    module.add_class::<python_native_builtin_blocks::PythonH264EncoderBlock>()?;
    module.add_class::<python_native_builtin_blocks::PythonH264DecoderBlock>()?;
    module.add_class::<python_native_builtin_blocks::PythonH265EncoderBlock>()?;
    module.add_class::<python_native_builtin_blocks::PythonH265DecoderBlock>()?;
    module.add_class::<python_native_builtin_blocks::PythonOpusEncoderBlock>()?;
    module.add_class::<python_native_builtin_blocks::PythonOpusDecoderBlock>()?;
    module.add_class::<python_native_builtin_blocks::PythonMp4SinkBlock>()?;
    module.add_class::<python_test_harness_endpoints::PythonTestBagFeederBlock>()?;
    module.add_class::<python_test_harness_endpoints::PythonTestBagCollectorBlock>()?;
    module.add_class::<python_added_processor::PythonAddedProcessor>()?;
    module.add_class::<python_added_processor::PythonProcessorOutputPortReference>()?;
    module.add_class::<python_added_processor::PythonProcessorInputPortReference>()?;
    module.add_class::<python_processor_link_data_access::PythonProcessorLinkDataAccess>()?;
    module.add_class::<python_processor_context::PythonRuntimeContextFullAccess>()?;
    module.add_class::<python_processor_context::PythonRuntimeContextLimitedAccess>()?;
    module.add_class::<python_processor_context::PythonGpuContextFullAccess>()?;
    module.add_class::<python_processor_context::PythonGpuContextLimitedAccess>()?;
    module.add_class::<python_processor_context::PythonGpuSurfaceHandle>()?;
    module.add_class::<python_processor_context::PythonGpuSurfaceDeviceTensorScope>()?;
    module.add_class::<python_processor_context::PythonGpuSurfaceCheckOutLease>()?;
    module.add_class::<python_processor_context::PythonOpaqueFdTextureExport>()?;
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
        python_capability_extension_host::capability_extension_host_for_the_app_process,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_capability_extension_host::capability_extension_host_for_the_helper_process,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        python_capability_extension_host::hand_loaded_capability_extensions_to_the_runtime,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(python_logging::monotonic_now_ns, module)?)?;
    module.add_function(wrap_pyfunction!(python_logging::log_event, module)?)?;
    module.add_function(wrap_pyfunction!(
        python_logging::runtime_log_directory,
        module
    )?)?;
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
