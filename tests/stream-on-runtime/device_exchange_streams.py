# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The streams `test_device_exchange.py` and the CUDA adapter's foreign-consumer test start on `tatolabd`.

A frame probe reports on its first frame, so its stream is a native test
pattern into the probe. A standalone probe acquires what it needs and reports
from `setup`, so its stream is the probe alone. The lagged consumer reads a
real camera, whose capture ring and pool are what it holds a frame against.
"""

import tatolab.stream
from tatolab.stream import StreamBuilder, stream

import device_exchange_probes
from camera_under_test import camera_source_config


def _add_a_test_pattern_into(stream_builder: StreamBuilder, frame_probe_class: type) -> None:
    pattern = stream_builder.add(
        tatolab.stream.TestPatternSource,
        config={
            "width": device_exchange_probes.SURFACE_WIDTH,
            "height": device_exchange_probes.SURFACE_HEIGHT,
        },
    )
    probe = stream_builder.add(frame_probe_class)
    stream_builder.connect(pattern.output("video"), probe.input("video_from_upstream"))


@stream
def camera_into_the_lagged_consumer_probe(stream_builder: StreamBuilder) -> None:
    """The camera's ring re-registers a different texture under one surface id
    every frame, and its pool recycles a slot every few frames — the two ways
    the pixels under a published id used to change underneath a reader."""
    camera = stream_builder.add(tatolab.stream.CameraSource, config=camera_source_config())
    probe = stream_builder.add(device_exchange_probes.LaggedConsumerHoldsItsFrameProbe)
    stream_builder.connect(camera.output("video"), probe.input("video_from_upstream"))


@stream
def a_test_pattern_into_graph_frame_to_torch_probe(stream_builder: StreamBuilder) -> None:
    _add_a_test_pattern_into(stream_builder, device_exchange_probes.GraphFrameToTorchProbe)


@stream
def a_test_pattern_into_device_edit_probe(stream_builder: StreamBuilder) -> None:
    _add_a_test_pattern_into(stream_builder, device_exchange_probes.DeviceEditProbe)


@stream
def a_test_pattern_into_with_block_edit_probe(stream_builder: StreamBuilder) -> None:
    _add_a_test_pattern_into(stream_builder, device_exchange_probes.WithBlockEditProbe)


@stream
def a_test_pattern_into_tensor_outlives_handle_probe(stream_builder: StreamBuilder) -> None:
    _add_a_test_pattern_into(stream_builder, device_exchange_probes.TensorOutlivesHandleProbe)


@stream
def a_test_pattern_into_host_side_probe(stream_builder: StreamBuilder) -> None:
    _add_a_test_pattern_into(stream_builder, device_exchange_probes.HostSideProbe)


@stream
def a_test_pattern_into_copy_request_refused_at_both_doors_probe(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into(
        stream_builder, device_exchange_probes.CopyRequestRefusedAtBothDoorsProbe
    )


@stream
def a_test_pattern_into_pixel_buffer_scope_discards_on_raise_probe(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into(
        stream_builder, device_exchange_probes.PixelBufferScopeDiscardsOnRaiseProbe
    )


@stream
def a_test_pattern_into_mlx_reads_the_frame_probe(stream_builder: StreamBuilder) -> None:
    _add_a_test_pattern_into(stream_builder, device_exchange_probes.MlxReadsTheFrameProbe)


@stream
def a_test_pattern_into_mlx_writes_the_frame_through_the_write_door_probe(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into(
        stream_builder, device_exchange_probes.MlxWritesTheFrameThroughTheWriteDoorProbe
    )


@stream
def a_test_pattern_into_mlx_write_with_a_view_alive_misses_the_frame_probe(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into(
        stream_builder, device_exchange_probes.MlxWriteWithAViewAliveMissesTheFrameProbe
    )


@stream
def a_test_pattern_into_mlx_whole_array_assignment_misses_the_frame_probe(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into(
        stream_builder, device_exchange_probes.MlxWholeArrayAssignmentMissesTheFrameProbe
    )


@stream
def dma_buf_export_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.DmaBufExportProbe)


@stream
def privileged_capability_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.PrivilegedCapabilityProbe)


@stream
def texture_handle_round_trip_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.TextureHandleRoundTripProbe)


@stream
def opaque_fd_export_handoff_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.OpaqueFdExportHandoffProbe)


@stream
def device_tensor_scope_doubles_a_kernel_output_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.DeviceTensorScopeDoublesAKernelOutputProbe)


@stream
def device_tensor_scope_discards_on_raise_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.DeviceTensorScopeDiscardsOnRaiseProbe)


@stream
def pooled_texture_export_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.PooledTextureExportProbe)


@stream
def device_tensor_scope_takes_every_acquired_texture_probe_alone(
    stream_builder: StreamBuilder,
) -> None:
    stream_builder.add(device_exchange_probes.DeviceTensorScopeTakesEveryAcquiredTextureProbe)


@stream
def device_tensor_strides_follow_the_row_pitch_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.DeviceTensorStridesFollowTheRowPitchProbe)


@stream
def torch_device_write_then_engine_gpu_read_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.TorchDeviceWriteThenEngineGpuReadProbe)


@stream
def mlx_device_write_then_engine_gpu_read_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.MlxDeviceWriteThenEngineGpuReadProbe)


@stream
def torch_and_mlx_scopes_alternate_in_one_helper_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.TorchAndMlxScopesAlternateInOneHelperProbe)


@stream
def torch_tensor_outlives_texture_handle_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.TorchTensorOutlivesTextureHandleProbe)


@stream
def mlx_array_outlives_texture_handle_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.MlxArrayOutlivesTextureHandleProbe)


@stream
def iosurface_export_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.IOSurfaceExportProbe)


@stream
def raw_handle_off_its_platform_refuses_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.RawHandleOffItsPlatformRefusesProbe)
