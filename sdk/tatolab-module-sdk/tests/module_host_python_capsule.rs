// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading the table out of a capsule, as a module's Python binding does.

#![cfg(feature = "pyo3")]

use std::ffi::c_void;
use std::ptr::NonNull;

use pyo3::prelude::*;
use pyo3::types::PyCapsule;
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_ABI_MINOR, MODULE_HOST_TABLE_STATUS_REFUSED,
    ModuleHostTableAudioBlockRequestedByDeviceCallback, ModuleHostTableAudioCaptureStreamRequest,
    ModuleHostTableAudioPlaybackStreamRequest, ModuleHostTableAudioStreamFormat,
    ModuleHostTableAudioStreamHandle, ModuleHostTableBytes,
    ModuleHostTableCapturedAudioBlockCallback, ModuleHostTableErrorMessage,
    ModuleHostTableIOSurfaceImport, ModuleHostTableOpenedAudioPlaybackStream,
    ModuleHostTableStatus, ModuleHostTableSurfaceHandle, ModuleHostTableUtf8,
    ModuleHostTableVirtualCameraStreamHandle,
};
use tatolab_module_sdk::{MODULE_HOST_TABLE_PYTHON_CAPSULE_NAME, ModuleHost, ModuleHostTable};

unsafe extern "C" fn do_nothing(_node_host: *mut c_void) {}
unsafe extern "C" fn refuse_open(
    _: *mut c_void,
    _: ModuleHostTableUtf8,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTablePortHandle,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTableErrorMessage,
) -> i32 {
    -1
}
unsafe extern "C" fn refuse_write(
    _: *mut c_void,
    _: tatolab_module_sdk::module_host_table_abi::ModuleHostTablePortHandle,
    _: tatolab_module_sdk::module_host_table_abi::ModuleHostTableBytes,
    _: i64,
    _: u32,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTableErrorMessage,
) -> i32 {
    -1
}
unsafe extern "C" fn refuse_entry(
    _: *mut c_void,
    _: tatolab_module_sdk::module_host_table_abi::ModuleHostTablePortHandle,
    _: ModuleHostTableUtf8,
    _: ModuleHostTableUtf8,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTableErrorMessage,
) -> i32 {
    -1
}
unsafe extern "C" fn refuse_read(
    _: *mut c_void,
    _: tatolab_module_sdk::module_host_table_abi::ModuleHostTablePortHandle,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTableReadMessage,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTableErrorMessage,
) -> i32 {
    -1
}
unsafe extern "C" fn release_nothing(
    _: *mut c_void,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTableReadMessage,
) {
}
unsafe extern "C" fn time_out(
    _: *mut c_void,
    _: u64,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTableErrorMessage,
) -> i32 {
    2
}
unsafe extern "C" fn never_stop(_: *mut c_void) -> u32 {
    0
}
unsafe extern "C" fn zero_clock(_: *mut c_void) -> i64 {
    0
}
unsafe extern "C" fn drop_log(
    _: *mut c_void,
    _: u32,
    _: ModuleHostTableUtf8,
    _: ModuleHostTableUtf8,
) {
}
unsafe extern "C" fn no_capabilities(_: *mut c_void) -> ModuleHostTableUtf8 {
    ModuleHostTableUtf8::of("{}")
}
unsafe extern "C" fn refuse_import(
    _: *mut c_void,
    _: *const tatolab_module_sdk::module_host_table_abi::ModuleHostTableDmaBufImport,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTableSurfaceHandle,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTableErrorMessage,
) -> i32 {
    -1
}
unsafe extern "C" fn refuse_iosurface_import(
    _: *mut c_void,
    _: *const ModuleHostTableIOSurfaceImport,
    _: *mut ModuleHostTableSurfaceHandle,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}
unsafe extern "C" fn refuse_lend_by_surface_id(
    _: *mut c_void,
    _: tatolab_module_sdk::module_host_table_abi::ModuleHostTableUtf8,
    _: *mut ModuleHostTableSurfaceHandle,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}
unsafe extern "C" fn refuse_lease(
    _: *mut c_void,
    _: ModuleHostTableUtf8,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTableSharedNativeServiceLease,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}
unsafe extern "C" fn refuse_lease_state(
    _: *mut c_void,
    _: tatolab_module_sdk::module_host_table_abi::ModuleHostTableSharedNativeServiceLeaseHandle,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}
unsafe extern "C" fn release_no_lease(
    _: *mut c_void,
    _: tatolab_module_sdk::module_host_table_abi::ModuleHostTableSharedNativeServiceLeaseHandle,
) {
}
unsafe extern "C" fn refuse_node_failure(
    _: *mut c_void,
    _: ModuleHostTableUtf8,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}
unsafe extern "C" fn refuse_acquire(
    _: *mut c_void,
    _: *const tatolab_module_sdk::module_host_table_abi::ModuleHostTableNodeOutputTextureRequest,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTableSurfaceHandle,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTableErrorMessage,
) -> i32 {
    -1
}
unsafe extern "C" fn refuse_convert(
    _: *mut c_void,
    _: tatolab_module_sdk::module_host_table_abi::ModuleHostTableSurfaceHandle,
    _: tatolab_module_sdk::module_host_table_abi::ModuleHostTableSurfaceHandle,
    _: *const tatolab_module_sdk::module_host_table_abi::ModuleHostTableColorInfo,
    _: *mut tatolab_module_sdk::module_host_table_abi::ModuleHostTableErrorMessage,
) -> i32 {
    -1
}
unsafe extern "C" fn no_surface_id(
    _: *mut c_void,
    _: tatolab_module_sdk::module_host_table_abi::ModuleHostTableSurfaceHandle,
) -> ModuleHostTableUtf8 {
    ModuleHostTableUtf8::of("")
}
unsafe extern "C" fn release_no_surface(
    _: *mut c_void,
    _: tatolab_module_sdk::module_host_table_abi::ModuleHostTableSurfaceHandle,
) {
}

unsafe extern "C" fn refuse_host_memory_write(
    _: *mut c_void,
    _: tatolab_module_sdk::module_host_table_abi::ModuleHostTableSurfaceHandle,
    _: *const tatolab_module_sdk::module_host_table_abi::ModuleHostTableHostMemoryPixels,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}

unsafe extern "C" fn refuse_host_memory_read(
    _: *mut c_void,
    _: tatolab_module_sdk::module_host_table_abi::ModuleHostTableSurfaceHandle,
    _: *const tatolab_module_sdk::module_host_table_abi::ModuleHostTableHostMemoryPixelsToFill,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}

unsafe extern "C" fn refuse_audio_capture_open(
    _: *mut c_void,
    _: *const ModuleHostTableAudioCaptureStreamRequest,
    _: *mut ModuleHostTableAudioStreamHandle,
    _: *mut ModuleHostTableAudioStreamFormat,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}
unsafe extern "C" fn refuse_audio_capture_start(
    _: *mut c_void,
    _: ModuleHostTableAudioStreamHandle,
    _: ModuleHostTableCapturedAudioBlockCallback,
    _: *mut c_void,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}
unsafe extern "C" fn refuse_audio_capture_verb(
    _: *mut c_void,
    _: ModuleHostTableAudioStreamHandle,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}
unsafe extern "C" fn close_nothing(_: *mut c_void, _: ModuleHostTableAudioStreamHandle) {}
unsafe extern "C" fn refuse_audio_playback_open(
    _: *mut c_void,
    _: *const ModuleHostTableAudioPlaybackStreamRequest,
    _: *mut ModuleHostTableAudioStreamHandle,
    _: *mut ModuleHostTableOpenedAudioPlaybackStream,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}
unsafe extern "C" fn refuse_audio_playback_start(
    _: *mut c_void,
    _: ModuleHostTableAudioStreamHandle,
    _: ModuleHostTableAudioBlockRequestedByDeviceCallback,
    _: *mut c_void,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}

unsafe extern "C" fn refuse_virtual_camera_open(
    _: *mut c_void,
    _: ModuleHostTableUtf8,
    _: *mut ModuleHostTableVirtualCameraStreamHandle,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}
unsafe extern "C" fn refuse_virtual_camera_offer_extent(
    _: *mut c_void,
    _: ModuleHostTableVirtualCameraStreamHandle,
    _: u32,
    _: u32,
    _: u32,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}
unsafe extern "C" fn refuse_virtual_camera_write(
    _: *mut c_void,
    _: ModuleHostTableVirtualCameraStreamHandle,
    _: ModuleHostTableBytes,
    _: u32,
    _: i64,
    _: *mut u32,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}
unsafe extern "C" fn refuse_virtual_camera_liveness(
    _: *mut c_void,
    _: ModuleHostTableVirtualCameraStreamHandle,
    _: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_REFUSED
}
unsafe extern "C" fn close_no_virtual_camera(
    _: *mut c_void,
    _: ModuleHostTableVirtualCameraStreamHandle,
) {
}

fn leaked_table_of_minor(abi_minor: u32) -> NonNull<c_void> {
    let table = Box::leak(Box::new(ModuleHostTable {
        abi_major: 1,
        abi_minor,
        table_size_bytes: std::mem::size_of::<ModuleHostTable>() as u64,
        node_host: std::ptr::null_mut(),
        node_host_retain: do_nothing,
        node_host_release: do_nothing,
        output_port_open: refuse_open,
        output_port_write: refuse_write,
        output_port_set_catalog_entry: refuse_entry,
        input_port_open: refuse_open,
        input_port_read: refuse_read,
        input_message_release: release_nothing,
        input_wait_for_any_port: time_out,
        stop_requested: never_stop,
        monotonic_now_ns: zero_clock,
        log: drop_log,
        host_capabilities_json: no_capabilities,
        audio_capture_stream_open: refuse_audio_capture_open,
        audio_capture_stream_start: refuse_audio_capture_start,
        audio_capture_stream_stop: refuse_audio_capture_verb,
        audio_capture_stream_liveness: refuse_audio_capture_verb,
        audio_capture_stream_close: close_nothing,
        surface_import_dma_buf: refuse_import,
        output_texture_acquire_from_node_output_pool: refuse_acquire,
        surface_convert_into: refuse_convert,
        surface_id: no_surface_id,
        surface_release: release_no_surface,
        audio_playback_stream_open: refuse_audio_playback_open,
        audio_playback_stream_start: refuse_audio_playback_start,
        audio_playback_stream_stop: refuse_audio_capture_verb,
        audio_playback_stream_liveness: refuse_audio_capture_verb,
        audio_playback_stream_close: close_nothing,
        surface_import_iosurface: refuse_iosurface_import,
        surface_write_from_host_memory: refuse_host_memory_write,
        surface_lend_by_surface_id: refuse_lend_by_surface_id,
        shared_native_service_lease_acquire: refuse_lease,
        shared_native_service_lease_state: refuse_lease_state,
        shared_native_service_lease_release: release_no_lease,
        node_fail_unrecoverably: refuse_node_failure,
        virtual_camera_stream_open: refuse_virtual_camera_open,
        virtual_camera_stream_offer_extent: refuse_virtual_camera_offer_extent,
        virtual_camera_stream_write_yuyv_frame: refuse_virtual_camera_write,
        virtual_camera_stream_liveness: refuse_virtual_camera_liveness,
        virtual_camera_stream_close: close_no_virtual_camera,
        surface_read_into_host_memory: refuse_host_memory_read,
    }));
    NonNull::from(table).cast()
}

#[test]
fn the_capsule_a_context_hands_over_reads_as_its_table() {
    Python::attach(|python| {
        let capsule = unsafe {
            PyCapsule::new_with_pointer(
                python,
                leaked_table_of_minor(MODULE_HOST_TABLE_ABI_MINOR),
                MODULE_HOST_TABLE_PYTHON_CAPSULE_NAME,
            )
        }
        .unwrap();
        let module_host =
            ModuleHost::from_python_module_host_table_capsule(capsule.as_any()).unwrap();
        assert_eq!(
            module_host.module_host_table_version(),
            (1, MODULE_HOST_TABLE_ABI_MINOR)
        );
    });
}

#[test]
fn a_capsule_of_another_name_is_refused_naming_the_capsule_it_wanted() {
    Python::attach(|python| {
        let capsule = unsafe {
            PyCapsule::new_with_pointer(
                python,
                leaked_table_of_minor(MODULE_HOST_TABLE_ABI_MINOR),
                c"numpy.core._ARRAY_API",
            )
        }
        .unwrap();
        let refusal =
            ModuleHost::from_python_module_host_table_capsule(capsule.as_any()).unwrap_err();
        assert!(
            refusal
                .to_string()
                .contains("tatolab.runtime.module_host_table"),
            "{refusal}"
        );
    });
}

#[test]
fn an_object_that_is_not_a_capsule_is_refused_naming_its_type() {
    Python::attach(|python| {
        let not_a_capsule = 3_i64.into_pyobject(python).unwrap();
        let refusal =
            ModuleHost::from_python_module_host_table_capsule(not_a_capsule.as_any()).unwrap_err();
        assert!(refusal.to_string().contains("got a int"), "{refusal}");
    });
}
