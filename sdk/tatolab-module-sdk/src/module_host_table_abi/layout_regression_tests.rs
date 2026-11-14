// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::mem::{align_of, offset_of, size_of};

const POINTER: usize = size_of::<*const c_void>();

#[test]
fn utf8_and_bytes_are_a_pointer_and_a_length() {
    for (size, align, bytes_offset, length_offset) in [
        (
            size_of::<ModuleHostTableUtf8>(),
            align_of::<ModuleHostTableUtf8>(),
            offset_of!(ModuleHostTableUtf8, bytes),
            offset_of!(ModuleHostTableUtf8, length),
        ),
        (
            size_of::<ModuleHostTableBytes>(),
            align_of::<ModuleHostTableBytes>(),
            offset_of!(ModuleHostTableBytes, bytes),
            offset_of!(ModuleHostTableBytes, length),
        ),
    ] {
        assert_eq!(size, 2 * POINTER);
        assert_eq!(align, POINTER);
        assert_eq!(bytes_offset, 0);
        assert_eq!(length_offset, POINTER);
    }
}

#[test]
fn a_port_handle_is_one_u64() {
    assert_eq!(size_of::<ModuleHostTablePortHandle>(), 8);
    assert_eq!(align_of::<ModuleHostTablePortHandle>(), 8);
}

#[test]
fn an_error_message_is_its_buffer_then_its_length() {
    assert_eq!(offset_of!(ModuleHostTableErrorMessage, bytes), 0);
    assert_eq!(offset_of!(ModuleHostTableErrorMessage, length), 1024);
    assert_eq!(size_of::<ModuleHostTableErrorMessage>(), 1028);
    assert_eq!(align_of::<ModuleHostTableErrorMessage>(), 4);
}

#[test]
fn a_read_message_lays_out_on_8_byte_fields() {
    assert_eq!(offset_of!(ModuleHostTableReadMessage, payload), 0);
    assert_eq!(
        offset_of!(ModuleHostTableReadMessage, payload_length),
        POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableReadMessage, timestamp_ns),
        2 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableReadMessage, starts_group),
        2 * POINTER + 8
    );
    assert_eq!(
        offset_of!(ModuleHostTableReadMessage, reserved),
        2 * POINTER + 12
    );
    assert_eq!(
        offset_of!(ModuleHostTableReadMessage, lost_since_last_read),
        2 * POINTER + 16
    );
    assert_eq!(
        offset_of!(ModuleHostTableReadMessage, host_owned_message),
        2 * POINTER + 24
    );
    assert_eq!(size_of::<ModuleHostTableReadMessage>(), 3 * POINTER + 24);
}

/// Every v1.0 field at the offset a module built against v1.0 reads it
/// from: the header, then one pointer-sized slot per field in order.
#[test]
fn the_v1_0_table_puts_every_field_where_a_v1_0_module_reads_it() {
    assert_eq!(offset_of!(ModuleHostTable, abi_major), 0);
    assert_eq!(offset_of!(ModuleHostTable, abi_minor), 4);
    assert_eq!(offset_of!(ModuleHostTable, table_size_bytes), 8);
    let pointer_slots_in_order = [
        offset_of!(ModuleHostTable, node_host),
        offset_of!(ModuleHostTable, node_host_retain),
        offset_of!(ModuleHostTable, node_host_release),
        offset_of!(ModuleHostTable, output_port_open),
        offset_of!(ModuleHostTable, output_port_write),
        offset_of!(ModuleHostTable, output_port_set_stream_facts),
        offset_of!(ModuleHostTable, input_port_open),
        offset_of!(ModuleHostTable, input_port_read),
        offset_of!(ModuleHostTable, input_message_release),
        offset_of!(ModuleHostTable, input_wait_for_any_port),
        offset_of!(ModuleHostTable, stop_requested),
        offset_of!(ModuleHostTable, monotonic_now_ns),
        offset_of!(ModuleHostTable, log),
        offset_of!(ModuleHostTable, host_capabilities_json),
    ];
    for (slot, offset) in pointer_slots_in_order.into_iter().enumerate() {
        assert_eq!(offset, 16 + slot * POINTER, "pointer slot {slot}");
    }
}

/// Minor 1 appends its audio capture verbs after every v1.0 field.
#[test]
fn the_v1_1_table_appends_its_audio_capture_verbs_after_every_v1_0_field() {
    let pointer_slots_appended_at_minor_1 = [
        offset_of!(ModuleHostTable, audio_capture_stream_open),
        offset_of!(ModuleHostTable, audio_capture_stream_start),
        offset_of!(ModuleHostTable, audio_capture_stream_stop),
        offset_of!(ModuleHostTable, audio_capture_stream_liveness),
        offset_of!(ModuleHostTable, audio_capture_stream_close),
    ];
    for (slot, offset) in pointer_slots_appended_at_minor_1.into_iter().enumerate() {
        assert_eq!(offset, 16 + (14 + slot) * POINTER, "minor 1 slot {slot}");
    }
}

/// Minor 3 appends its audio playback verbs after every v1.2 field.
#[test]
fn the_v1_3_table_appends_its_audio_playback_verbs_after_every_v1_2_field() {
    let pointer_slots_appended_at_minor_3 = [
        offset_of!(ModuleHostTable, audio_playback_stream_open),
        offset_of!(ModuleHostTable, audio_playback_stream_start),
        offset_of!(ModuleHostTable, audio_playback_stream_stop),
        offset_of!(ModuleHostTable, audio_playback_stream_liveness),
        offset_of!(ModuleHostTable, audio_playback_stream_close),
    ];
    for (slot, offset) in pointer_slots_appended_at_minor_3.into_iter().enumerate() {
        assert_eq!(offset, 16 + (24 + slot) * POINTER, "minor 3 slot {slot}");
    }
    assert_eq!(align_of::<ModuleHostTable>(), 8);
}

#[test]
fn a_playback_request_is_the_device_id_then_the_format() {
    assert_eq!(
        offset_of!(ModuleHostTableAudioPlaybackStreamRequest, device_id),
        0
    );
    assert_eq!(
        offset_of!(ModuleHostTableAudioPlaybackStreamRequest, requested_format),
        2 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableAudioPlaybackStreamRequest, reserved),
        2 * POINTER + 12
    );
    assert_eq!(
        size_of::<ModuleHostTableAudioPlaybackStreamRequest>(),
        2 * POINTER + 16
    );
    assert_eq!(
        align_of::<ModuleHostTableAudioPlaybackStreamRequest>(),
        POINTER
    );
}

#[test]
fn an_opened_playback_stream_is_its_format_then_its_device_period() {
    assert_eq!(
        offset_of!(ModuleHostTableOpenedAudioPlaybackStream, stream_format),
        0
    );
    assert_eq!(
        offset_of!(
            ModuleHostTableOpenedAudioPlaybackStream,
            device_period_in_per_channel_samples
        ),
        12
    );
    assert_eq!(size_of::<ModuleHostTableOpenedAudioPlaybackStream>(), 16);
    assert_eq!(align_of::<ModuleHostTableOpenedAudioPlaybackStream>(), 4);
}

#[test]
fn a_block_requested_by_a_device_lays_out_on_8_byte_fields() {
    assert_eq!(
        offset_of!(
            ModuleHostTableAudioBlockRequestedByDevice,
            interleaved_sample_bytes_to_fill
        ),
        0
    );
    assert_eq!(
        offset_of!(
            ModuleHostTableAudioBlockRequestedByDevice,
            interleaved_sample_byte_count
        ),
        POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableAudioBlockRequestedByDevice, sample_count),
        2 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableAudioBlockRequestedByDevice, reserved),
        2 * POINTER + 4
    );
    assert_eq!(
        size_of::<ModuleHostTableAudioBlockRequestedByDevice>(),
        2 * POINTER + 8
    );
}

#[test]
fn an_audio_stream_format_is_three_u32s() {
    assert_eq!(offset_of!(ModuleHostTableAudioStreamFormat, sample_rate), 0);
    assert_eq!(offset_of!(ModuleHostTableAudioStreamFormat, channels), 4);
    assert_eq!(
        offset_of!(ModuleHostTableAudioStreamFormat, sample_format),
        8
    );
    assert_eq!(size_of::<ModuleHostTableAudioStreamFormat>(), 12);
    assert_eq!(align_of::<ModuleHostTableAudioStreamFormat>(), 4);
}

#[test]
fn a_capture_request_is_the_device_id_then_the_format() {
    assert_eq!(
        offset_of!(ModuleHostTableAudioCaptureStreamRequest, device_id),
        0
    );
    assert_eq!(
        offset_of!(ModuleHostTableAudioCaptureStreamRequest, requested_format),
        2 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableAudioCaptureStreamRequest, reserved),
        2 * POINTER + 12
    );
    assert_eq!(
        size_of::<ModuleHostTableAudioCaptureStreamRequest>(),
        2 * POINTER + 16
    );
    assert_eq!(
        align_of::<ModuleHostTableAudioCaptureStreamRequest>(),
        POINTER
    );
}

/// Minor 2 appends its surface verbs after the minor 1 audio capture verbs.
#[test]
fn the_v1_2_table_appends_its_surface_verbs_after_every_v1_1_field() {
    let pointer_slots_in_order = [
        offset_of!(ModuleHostTable, surface_import_dma_buf),
        offset_of!(
            ModuleHostTable,
            output_texture_acquire_from_node_output_pool
        ),
        offset_of!(ModuleHostTable, surface_convert_into),
        offset_of!(ModuleHostTable, surface_id),
        offset_of!(ModuleHostTable, surface_release),
    ];
    for (slot, offset) in pointer_slots_in_order.into_iter().enumerate() {
        assert_eq!(offset, 16 + (19 + slot) * POINTER, "pointer slot {slot}");
    }
}

/// Minor 4 appends its IOSurface import after the minor 3 audio playback verbs.
#[test]
fn the_v1_4_table_appends_its_iosurface_import_after_every_v1_3_field() {
    assert_eq!(
        offset_of!(ModuleHostTable, surface_import_iosurface),
        16 + 29 * POINTER
    );
}

/// Minor 5 appends its host-memory write after the minor 4 IOSurface import.
#[test]
fn the_v1_5_table_appends_its_host_memory_write_after_every_v1_4_field() {
    assert_eq!(
        offset_of!(ModuleHostTable, surface_write_from_host_memory),
        16 + 30 * POINTER
    );
}

/// Minor 6 appends its lend by surface id after the minor 5 host-memory write.
#[test]
fn the_v1_6_table_appends_its_lend_by_surface_id_after_every_v1_5_field() {
    assert_eq!(
        offset_of!(ModuleHostTable, surface_lend_by_surface_id),
        16 + 31 * POINTER
    );
}

/// Minor 7 appends its shared native service leases and node failure
/// after the minor 6 lend.
#[test]
fn the_v1_7_table_appends_its_service_leases_and_node_failure_after_every_v1_6_field() {
    assert_eq!(
        offset_of!(ModuleHostTable, shared_native_service_lease_acquire),
        16 + 32 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTable, shared_native_service_lease_state),
        16 + 33 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTable, shared_native_service_lease_release),
        16 + 34 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTable, node_fail_unrecoverably),
        16 + 35 * POINTER
    );
}

#[test]
fn a_service_lease_is_its_handle_then_its_endpoint_then_its_tenant() {
    assert_eq!(
        size_of::<ModuleHostTableSharedNativeServiceLeaseHandle>(),
        8
    );
    assert_eq!(
        offset_of!(ModuleHostTableSharedNativeServiceLease, lease),
        0
    );
    assert_eq!(
        offset_of!(
            ModuleHostTableSharedNativeServiceLease,
            endpoint_socket_path
        ),
        8
    );
    assert_eq!(
        offset_of!(ModuleHostTableSharedNativeServiceLease, tenant_stream_id),
        8 + 2 * POINTER
    );
    assert_eq!(
        size_of::<ModuleHostTableSharedNativeServiceLease>(),
        8 + 4 * POINTER
    );
}

/// Minor 8 appends its virtual camera verbs after the minor 7 node failure.
#[test]
fn the_v1_8_table_appends_its_virtual_camera_verbs_after_every_v1_7_field() {
    let pointer_slots_appended_at_minor_8 = [
        offset_of!(ModuleHostTable, virtual_camera_stream_open),
        offset_of!(ModuleHostTable, virtual_camera_stream_offer_extent),
        offset_of!(ModuleHostTable, virtual_camera_stream_write_yuyv_frame),
        offset_of!(ModuleHostTable, virtual_camera_stream_liveness),
        offset_of!(ModuleHostTable, virtual_camera_stream_close),
    ];
    for (slot, offset) in pointer_slots_appended_at_minor_8.into_iter().enumerate() {
        assert_eq!(offset, 16 + (36 + slot) * POINTER, "minor 8 slot {slot}");
    }
}

/// Minor 9 appends its host-memory read after the minor 8 camera verbs.
#[test]
fn the_v1_9_table_appends_its_host_memory_read_after_every_v1_8_field() {
    assert_eq!(
        offset_of!(ModuleHostTable, surface_read_into_host_memory),
        16 + 41 * POINTER
    );
}

/// Minor 10 appends its stream-time conversion after the minor 9 read.
#[test]
fn the_v1_10_table_appends_its_stream_time_conversion_after_every_v1_9_field() {
    assert_eq!(
        offset_of!(ModuleHostTable, stream_time_ns_of_a_machine_instant),
        16 + 42 * POINTER
    );
}

/// Minor 11 appends its described reads and writes after the minor 10
/// conversion.
#[test]
fn the_v1_11_table_appends_its_described_reads_and_writes_after_every_v1_10_field() {
    let pointer_slots_appended_at_minor_11 = [
        offset_of!(ModuleHostTable, input_message_payload_descriptor),
        offset_of!(ModuleHostTable, input_message_lend_pooled_gpu_frame),
        offset_of!(ModuleHostTable, output_port_write_described),
        offset_of!(ModuleHostTable, surface_gpu_pool_slot_frame),
    ];
    for (slot, offset) in pointer_slots_appended_at_minor_11.into_iter().enumerate() {
        assert_eq!(offset, 16 + (43 + slot) * POINTER, "minor 11 slot {slot}");
    }
}

/// Minor 12 appends its given-up groups declaration after the minor 11
/// described reads and writes.
#[test]
fn the_v1_12_table_appends_its_given_up_groups_declaration_after_every_v1_11_field() {
    assert_eq!(
        offset_of!(
            ModuleHostTable,
            output_port_declare_groups_given_up_before_the_next_write
        ),
        16 + 47 * POINTER
    );
    assert_eq!(size_of::<ModuleHostTable>(), 16 + 48 * POINTER);
}

/// A payload descriptor lays out as the engine's 64-byte channel-sample
/// descriptor does.
#[test]
fn a_payload_descriptor_is_its_kind_then_its_60_byte_body() {
    assert_eq!(offset_of!(ModuleHostTablePayloadDescriptor, kind), 0);
    assert_eq!(offset_of!(ModuleHostTablePayloadDescriptor, body), 4);
    assert_eq!(size_of::<ModuleHostTablePayloadDescriptor>(), 64);
}

#[test]
fn pixels_to_fill_lay_out_as_the_pixels_a_write_hands() {
    assert_eq!(size_of::<ModuleHostTableMutableBytes>(), 2 * POINTER);
    assert_eq!(
        offset_of!(ModuleHostTableHostMemoryPixelsToFill, width),
        offset_of!(ModuleHostTableHostMemoryPixels, width)
    );
    assert_eq!(
        offset_of!(ModuleHostTableHostMemoryPixelsToFill, row_stride_bytes),
        offset_of!(ModuleHostTableHostMemoryPixels, row_stride_bytes)
    );
    assert_eq!(
        size_of::<ModuleHostTableHostMemoryPixelsToFill>(),
        size_of::<ModuleHostTableHostMemoryPixels>()
    );
}

#[test]
fn a_virtual_camera_handle_is_one_u64() {
    assert_eq!(size_of::<ModuleHostTableVirtualCameraStreamHandle>(), 8);
    assert_eq!(align_of::<ModuleHostTableVirtualCameraStreamHandle>(), 8);
}

#[test]
fn an_iosurface_import_puts_its_surface_then_its_name_then_its_extent() {
    assert_eq!(offset_of!(ModuleHostTableIOSurfaceImport, iosurface), 0);
    assert_eq!(
        offset_of!(ModuleHostTableIOSurfaceImport, pixel_format),
        POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableIOSurfaceImport, width),
        3 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableIOSurfaceImport, height),
        3 * POINTER + 4
    );
    assert_eq!(size_of::<ModuleHostTableIOSurfaceImport>(), 3 * POINTER + 8);
}

#[test]
fn host_memory_pixels_put_their_views_then_their_geometry() {
    assert_eq!(offset_of!(ModuleHostTableHostMemoryPixels, pixels), 0);
    assert_eq!(
        offset_of!(ModuleHostTableHostMemoryPixels, pixel_format),
        2 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableHostMemoryPixels, width),
        4 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableHostMemoryPixels, height),
        4 * POINTER + 4
    );
    assert_eq!(
        offset_of!(ModuleHostTableHostMemoryPixels, row_stride_bytes),
        4 * POINTER + 8
    );
    assert_eq!(
        size_of::<ModuleHostTableHostMemoryPixels>(),
        4 * POINTER + 16
    );
    assert_eq!(align_of::<ModuleHostTableHostMemoryPixels>(), 8);
}

#[test]
fn a_surface_handle_is_one_u64() {
    assert_eq!(size_of::<ModuleHostTableSurfaceHandle>(), 8);
    assert_eq!(align_of::<ModuleHostTableSurfaceHandle>(), 8);
}

#[test]
fn a_dma_buf_import_puts_its_geometry_then_its_name_then_its_planes() {
    assert_eq!(offset_of!(ModuleHostTableDmaBufImport, fd), 0);
    assert_eq!(offset_of!(ModuleHostTableDmaBufImport, width), 4);
    assert_eq!(offset_of!(ModuleHostTableDmaBufImport, height), 8);
    assert_eq!(offset_of!(ModuleHostTableDmaBufImport, plane_count), 12);
    assert_eq!(offset_of!(ModuleHostTableDmaBufImport, pixel_format), 16);
    let after_the_name = 16 + 2 * POINTER;
    assert_eq!(
        offset_of!(ModuleHostTableDmaBufImport, allocation_byte_size),
        after_the_name
    );
    assert_eq!(
        offset_of!(ModuleHostTableDmaBufImport, drm_format_modifier),
        after_the_name + 8
    );
    assert_eq!(
        offset_of!(ModuleHostTableDmaBufImport, plane_offsets_bytes),
        after_the_name + 16
    );
    assert_eq!(
        offset_of!(ModuleHostTableDmaBufImport, plane_strides_bytes),
        after_the_name + 48
    );
    assert_eq!(
        size_of::<ModuleHostTableDmaBufImport>(),
        after_the_name + 80
    );
}

#[test]
fn a_node_output_texture_request_puts_its_names_then_its_geometry() {
    assert_eq!(
        offset_of!(ModuleHostTableNodeOutputTextureRequest, pool_key),
        0
    );
    assert_eq!(
        offset_of!(ModuleHostTableNodeOutputTextureRequest, texture_format),
        2 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableNodeOutputTextureRequest, usage),
        4 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableNodeOutputTextureRequest, width),
        6 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableNodeOutputTextureRequest, reserved),
        6 * POINTER + 12
    );
    assert_eq!(
        size_of::<ModuleHostTableNodeOutputTextureRequest>(),
        6 * POINTER + 16
    );
}

#[test]
fn a_color_info_is_four_names() {
    assert_eq!(offset_of!(ModuleHostTableColorInfo, matrix), 4 * POINTER);
    assert_eq!(size_of::<ModuleHostTableColorInfo>(), 8 * POINTER);
}

#[test]
fn an_audio_stream_handle_is_one_u64() {
    assert_eq!(size_of::<ModuleHostTableAudioStreamHandle>(), 8);
    assert_eq!(align_of::<ModuleHostTableAudioStreamHandle>(), 8);
}

#[test]
fn a_captured_block_lays_out_on_8_byte_fields() {
    assert_eq!(
        offset_of!(ModuleHostTableCapturedAudioBlock, interleaved_sample_bytes),
        0
    );
    assert_eq!(
        offset_of!(ModuleHostTableCapturedAudioBlock, sample_count),
        2 * POINTER
    );
    assert_eq!(
        offset_of!(ModuleHostTableCapturedAudioBlock, reserved),
        2 * POINTER + 4
    );
    assert_eq!(
        offset_of!(ModuleHostTableCapturedAudioBlock, first_sample_timestamp_ns),
        2 * POINTER + 8
    );
    assert_eq!(
        size_of::<ModuleHostTableCapturedAudioBlock>(),
        2 * POINTER + 16
    );
}
