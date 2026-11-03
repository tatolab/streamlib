/* Copyright (c) 2025 Jonathan Fontanez
 * SPDX-License-Identifier: BUSL-1.1 */

/* The C header's layout, held to the same sizes and offsets the Rust mirror's
 * layout regression tests hold `module_host_table_abi.rs` to. */

#include "tatolab_module_host_table.h"

#include <stdalign.h>

#define POINTER sizeof(void *)

_Static_assert(sizeof(ModuleHostTableUtf8) == 2 * POINTER, "ModuleHostTableUtf8 size");
_Static_assert(alignof(ModuleHostTableUtf8) == POINTER, "ModuleHostTableUtf8 align");
_Static_assert(offsetof(ModuleHostTableUtf8, bytes) == 0, "ModuleHostTableUtf8.bytes");
_Static_assert(offsetof(ModuleHostTableUtf8, length) == POINTER, "ModuleHostTableUtf8.length");

_Static_assert(sizeof(ModuleHostTableBytes) == 2 * POINTER, "ModuleHostTableBytes size");
_Static_assert(alignof(ModuleHostTableBytes) == POINTER, "ModuleHostTableBytes align");
_Static_assert(offsetof(ModuleHostTableBytes, bytes) == 0, "ModuleHostTableBytes.bytes");
_Static_assert(offsetof(ModuleHostTableBytes, length) == POINTER, "ModuleHostTableBytes.length");

_Static_assert(sizeof(ModuleHostTablePortHandle) == 8, "ModuleHostTablePortHandle size");
_Static_assert(alignof(ModuleHostTablePortHandle) == 8, "ModuleHostTablePortHandle align");

_Static_assert(offsetof(ModuleHostTableErrorMessage, bytes) == 0, "ModuleHostTableErrorMessage.bytes");
_Static_assert(offsetof(ModuleHostTableErrorMessage, length) == 1024, "ModuleHostTableErrorMessage.length");
_Static_assert(sizeof(ModuleHostTableErrorMessage) == 1028, "ModuleHostTableErrorMessage size");
_Static_assert(alignof(ModuleHostTableErrorMessage) == 4, "ModuleHostTableErrorMessage align");

_Static_assert(offsetof(ModuleHostTableReadMessage, payload) == 0, "ModuleHostTableReadMessage.payload");
_Static_assert(offsetof(ModuleHostTableReadMessage, payload_length) == POINTER, "ModuleHostTableReadMessage.payload_length");
_Static_assert(offsetof(ModuleHostTableReadMessage, timestamp_ns) == 2 * POINTER, "ModuleHostTableReadMessage.timestamp_ns");
_Static_assert(offsetof(ModuleHostTableReadMessage, starts_group) == 2 * POINTER + 8, "ModuleHostTableReadMessage.starts_group");
_Static_assert(offsetof(ModuleHostTableReadMessage, reserved) == 2 * POINTER + 12, "ModuleHostTableReadMessage.reserved");
_Static_assert(offsetof(ModuleHostTableReadMessage, lost_since_last_read) == 2 * POINTER + 16, "ModuleHostTableReadMessage.lost_since_last_read");
_Static_assert(offsetof(ModuleHostTableReadMessage, host_owned_message) == 2 * POINTER + 24, "ModuleHostTableReadMessage.host_owned_message");
_Static_assert(sizeof(ModuleHostTableReadMessage) == 3 * POINTER + 24, "ModuleHostTableReadMessage size");

_Static_assert(offsetof(ModuleHostTableAudioStreamFormat, sample_rate) == 0, "ModuleHostTableAudioStreamFormat.sample_rate");
_Static_assert(offsetof(ModuleHostTableAudioStreamFormat, channels) == 4, "ModuleHostTableAudioStreamFormat.channels");
_Static_assert(offsetof(ModuleHostTableAudioStreamFormat, sample_format) == 8, "ModuleHostTableAudioStreamFormat.sample_format");
_Static_assert(sizeof(ModuleHostTableAudioStreamFormat) == 12, "ModuleHostTableAudioStreamFormat size");

_Static_assert(offsetof(ModuleHostTableAudioCaptureStreamRequest, requested_format) == 2 * POINTER, "ModuleHostTableAudioCaptureStreamRequest.requested_format");
_Static_assert(sizeof(ModuleHostTableAudioCaptureStreamRequest) == 2 * POINTER + 16, "ModuleHostTableAudioCaptureStreamRequest size");

_Static_assert(sizeof(ModuleHostTableAudioStreamHandle) == 8, "ModuleHostTableAudioStreamHandle size");

_Static_assert(offsetof(ModuleHostTableCapturedAudioBlock, sample_count) == 2 * POINTER, "ModuleHostTableCapturedAudioBlock.sample_count");
_Static_assert(offsetof(ModuleHostTableCapturedAudioBlock, first_sample_timestamp_ns) == 2 * POINTER + 8, "ModuleHostTableCapturedAudioBlock.first_sample_timestamp_ns");
_Static_assert(sizeof(ModuleHostTableCapturedAudioBlock) == 2 * POINTER + 16, "ModuleHostTableCapturedAudioBlock size");

_Static_assert(offsetof(ModuleHostTableAudioPlaybackStreamRequest, requested_format) == 2 * POINTER, "ModuleHostTableAudioPlaybackStreamRequest.requested_format");
_Static_assert(sizeof(ModuleHostTableAudioPlaybackStreamRequest) == 2 * POINTER + 16, "ModuleHostTableAudioPlaybackStreamRequest size");

_Static_assert(offsetof(ModuleHostTableOpenedAudioPlaybackStream, device_period_in_per_channel_samples) == 12, "ModuleHostTableOpenedAudioPlaybackStream.device_period_in_per_channel_samples");
_Static_assert(sizeof(ModuleHostTableOpenedAudioPlaybackStream) == 16, "ModuleHostTableOpenedAudioPlaybackStream size");

_Static_assert(offsetof(ModuleHostTableAudioBlockRequestedByDevice, interleaved_sample_byte_count) == POINTER, "ModuleHostTableAudioBlockRequestedByDevice.interleaved_sample_byte_count");
_Static_assert(offsetof(ModuleHostTableAudioBlockRequestedByDevice, sample_count) == 2 * POINTER, "ModuleHostTableAudioBlockRequestedByDevice.sample_count");
_Static_assert(sizeof(ModuleHostTableAudioBlockRequestedByDevice) == 2 * POINTER + 8, "ModuleHostTableAudioBlockRequestedByDevice size");

#define POINTER_SLOT(slot) (16 + (slot) * POINTER)

_Static_assert(offsetof(ModuleHostTable, abi_major) == 0, "ModuleHostTable.abi_major");
_Static_assert(offsetof(ModuleHostTable, abi_minor) == 4, "ModuleHostTable.abi_minor");
_Static_assert(offsetof(ModuleHostTable, table_size_bytes) == 8, "ModuleHostTable.table_size_bytes");
_Static_assert(offsetof(ModuleHostTable, node_host) == POINTER_SLOT(0), "ModuleHostTable.node_host");
_Static_assert(offsetof(ModuleHostTable, node_host_retain) == POINTER_SLOT(1), "ModuleHostTable.node_host_retain");
_Static_assert(offsetof(ModuleHostTable, node_host_release) == POINTER_SLOT(2), "ModuleHostTable.node_host_release");
_Static_assert(offsetof(ModuleHostTable, output_port_open) == POINTER_SLOT(3), "ModuleHostTable.output_port_open");
_Static_assert(offsetof(ModuleHostTable, output_port_write) == POINTER_SLOT(4), "ModuleHostTable.output_port_write");
_Static_assert(offsetof(ModuleHostTable, output_port_set_catalog_entry) == POINTER_SLOT(5), "ModuleHostTable.output_port_set_catalog_entry");
_Static_assert(offsetof(ModuleHostTable, input_port_open) == POINTER_SLOT(6), "ModuleHostTable.input_port_open");
_Static_assert(offsetof(ModuleHostTable, input_port_read) == POINTER_SLOT(7), "ModuleHostTable.input_port_read");
_Static_assert(offsetof(ModuleHostTable, input_message_release) == POINTER_SLOT(8), "ModuleHostTable.input_message_release");
_Static_assert(offsetof(ModuleHostTable, input_wait_for_any_port) == POINTER_SLOT(9), "ModuleHostTable.input_wait_for_any_port");
_Static_assert(offsetof(ModuleHostTable, stop_requested) == POINTER_SLOT(10), "ModuleHostTable.stop_requested");
_Static_assert(offsetof(ModuleHostTable, monotonic_now_ns) == POINTER_SLOT(11), "ModuleHostTable.monotonic_now_ns");
_Static_assert(offsetof(ModuleHostTable, log) == POINTER_SLOT(12), "ModuleHostTable.log");
_Static_assert(offsetof(ModuleHostTable, host_capabilities_json) == POINTER_SLOT(13), "ModuleHostTable.host_capabilities_json");
_Static_assert(offsetof(ModuleHostTable, audio_capture_stream_open) == POINTER_SLOT(14), "ModuleHostTable.audio_capture_stream_open");
_Static_assert(offsetof(ModuleHostTable, audio_capture_stream_start) == POINTER_SLOT(15), "ModuleHostTable.audio_capture_stream_start");
_Static_assert(offsetof(ModuleHostTable, audio_capture_stream_stop) == POINTER_SLOT(16), "ModuleHostTable.audio_capture_stream_stop");
_Static_assert(offsetof(ModuleHostTable, audio_capture_stream_liveness) == POINTER_SLOT(17), "ModuleHostTable.audio_capture_stream_liveness");
_Static_assert(offsetof(ModuleHostTable, audio_capture_stream_close) == POINTER_SLOT(18), "ModuleHostTable.audio_capture_stream_close");
_Static_assert(offsetof(ModuleHostTable, surface_import_dma_buf) == POINTER_SLOT(19), "ModuleHostTable.surface_import_dma_buf");
_Static_assert(offsetof(ModuleHostTable, output_texture_acquire_from_node_output_pool) == POINTER_SLOT(20), "ModuleHostTable.output_texture_acquire_from_node_output_pool");
_Static_assert(offsetof(ModuleHostTable, surface_convert_into) == POINTER_SLOT(21), "ModuleHostTable.surface_convert_into");
_Static_assert(offsetof(ModuleHostTable, surface_id) == POINTER_SLOT(22), "ModuleHostTable.surface_id");
_Static_assert(offsetof(ModuleHostTable, surface_release) == POINTER_SLOT(23), "ModuleHostTable.surface_release");
_Static_assert(offsetof(ModuleHostTable, audio_playback_stream_open) == POINTER_SLOT(24), "ModuleHostTable.audio_playback_stream_open");
_Static_assert(offsetof(ModuleHostTable, audio_playback_stream_start) == POINTER_SLOT(25), "ModuleHostTable.audio_playback_stream_start");
_Static_assert(offsetof(ModuleHostTable, audio_playback_stream_stop) == POINTER_SLOT(26), "ModuleHostTable.audio_playback_stream_stop");
_Static_assert(offsetof(ModuleHostTable, audio_playback_stream_liveness) == POINTER_SLOT(27), "ModuleHostTable.audio_playback_stream_liveness");
_Static_assert(offsetof(ModuleHostTable, audio_playback_stream_close) == POINTER_SLOT(28), "ModuleHostTable.audio_playback_stream_close");
_Static_assert(offsetof(ModuleHostTable, surface_import_iosurface) == POINTER_SLOT(29), "ModuleHostTable.surface_import_iosurface");
_Static_assert(offsetof(ModuleHostTable, surface_write_from_host_memory) == POINTER_SLOT(30), "ModuleHostTable.surface_write_from_host_memory");
_Static_assert(offsetof(ModuleHostTable, surface_lend_by_surface_id) == POINTER_SLOT(31), "ModuleHostTable.surface_lend_by_surface_id");
_Static_assert(sizeof(ModuleHostTable) == POINTER_SLOT(32), "ModuleHostTable size");
_Static_assert(alignof(ModuleHostTable) == 8, "ModuleHostTable align");

_Static_assert(sizeof(ModuleHostTableAudioStreamFormat) == 12, "ModuleHostTableAudioStreamFormat size");
_Static_assert(sizeof(ModuleHostTableAudioCaptureStreamRequest) == 2 * POINTER + 16, "ModuleHostTableAudioCaptureStreamRequest size");
_Static_assert(sizeof(ModuleHostTableAudioStreamHandle) == 8, "ModuleHostTableAudioStreamHandle size");
_Static_assert(sizeof(ModuleHostTableCapturedAudioBlock) == 2 * POINTER + 16, "ModuleHostTableCapturedAudioBlock size");

_Static_assert(sizeof(ModuleHostTableSurfaceHandle) == 8, "ModuleHostTableSurfaceHandle size");
_Static_assert(offsetof(ModuleHostTableDmaBufImport, allocation_byte_size) == 16 + 2 * POINTER, "ModuleHostTableDmaBufImport.allocation_byte_size");
_Static_assert(offsetof(ModuleHostTableDmaBufImport, plane_strides_bytes) == 64 + 2 * POINTER, "ModuleHostTableDmaBufImport.plane_strides_bytes");
_Static_assert(sizeof(ModuleHostTableDmaBufImport) == 96 + 2 * POINTER, "ModuleHostTableDmaBufImport size");
_Static_assert(offsetof(ModuleHostTableNodeOutputTextureRequest, width) == 6 * POINTER, "ModuleHostTableNodeOutputTextureRequest.width");
_Static_assert(sizeof(ModuleHostTableNodeOutputTextureRequest) == 6 * POINTER + 16, "ModuleHostTableNodeOutputTextureRequest size");
_Static_assert(offsetof(ModuleHostTableColorInfo, matrix) == 4 * POINTER, "ModuleHostTableColorInfo.matrix");
_Static_assert(sizeof(ModuleHostTableColorInfo) == 8 * POINTER, "ModuleHostTableColorInfo size");
_Static_assert(offsetof(ModuleHostTableIOSurfaceImport, width) == 3 * POINTER, "ModuleHostTableIOSurfaceImport.width");
_Static_assert(sizeof(ModuleHostTableIOSurfaceImport) == 3 * POINTER + 8, "ModuleHostTableIOSurfaceImport size");
_Static_assert(offsetof(ModuleHostTableHostMemoryPixels, width) == 4 * POINTER, "ModuleHostTableHostMemoryPixels.width");
_Static_assert(offsetof(ModuleHostTableHostMemoryPixels, row_stride_bytes) == 4 * POINTER + 8, "ModuleHostTableHostMemoryPixels.row_stride_bytes");
_Static_assert(sizeof(ModuleHostTableHostMemoryPixels) == 4 * POINTER + 16, "ModuleHostTableHostMemoryPixels size");

_Static_assert(MODULE_HOST_TABLE_ABI_MAJOR == 1, "MODULE_HOST_TABLE_ABI_MAJOR");
_Static_assert(MODULE_HOST_TABLE_ABI_MINOR == 6, "MODULE_HOST_TABLE_ABI_MINOR");
_Static_assert(MODULE_HOST_TABLE_STATUS_REFUSED < 0, "MODULE_HOST_TABLE_STATUS_REFUSED");
_Static_assert(MODULE_HOST_TABLE_TIMESTAMP_NOW == INT64_MIN, "MODULE_HOST_TABLE_TIMESTAMP_NOW");
