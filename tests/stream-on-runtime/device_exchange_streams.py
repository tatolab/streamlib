# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The streams `test_device_exchange.py` and the CUDA adapter's foreign-consumer test start on `tatolabd`.

A frame probe reports on its first frame, so its stream is a native test
pattern into the probe. A standalone probe acquires what it needs and reports
from `setup`, so its stream is the probe alone. The lagged consumer reads a
real camera, whose capture ring and pool are what it holds a frame against.
"""

from tatolab.camera import CameraSource
from tatolab.stream import StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource

import device_exchange_probes
from camera_under_test import camera_source_config


def _add_a_test_pattern_into(stream_builder: StreamBuilder, frame_probe_class: type) -> None:
    pattern = stream_builder.add(
        TestPatternSource,
        config={
            "width": device_exchange_probes.SURFACE_WIDTH,
            "height": device_exchange_probes.SURFACE_HEIGHT,
        },
    )
    probe = stream_builder.add(frame_probe_class)
    stream_builder.connect(pattern.output("video"), probe.input("video_from_upstream"))


@graph_generator
def camera_into_the_lagged_consumer_probe(stream_builder: StreamBuilder) -> None:
    """The camera's ring re-registers a different texture under one surface id
    every frame, and its pool recycles a slot every few frames — the two ways
    the pixels under a published id used to change underneath a reader."""
    camera = stream_builder.add(CameraSource, config=camera_source_config())
    probe = stream_builder.add(device_exchange_probes.LaggedConsumerHoldsItsFrameProbe)
    stream_builder.connect(camera.output("video"), probe.input("video_from_upstream"))


@graph_generator
def a_test_pattern_into_graph_frame_to_torch_probe(stream_builder: StreamBuilder) -> None:
    _add_a_test_pattern_into(stream_builder, device_exchange_probes.GraphFrameToTorchProbe)


@graph_generator
def a_test_pattern_into_device_edit_probe(stream_builder: StreamBuilder) -> None:
    _add_a_test_pattern_into(stream_builder, device_exchange_probes.DeviceEditProbe)


@graph_generator
def a_test_pattern_into_with_block_edit_probe(stream_builder: StreamBuilder) -> None:
    _add_a_test_pattern_into(stream_builder, device_exchange_probes.WithBlockEditProbe)


@graph_generator
def a_test_pattern_into_tensor_outlives_handle_probe(stream_builder: StreamBuilder) -> None:
    _add_a_test_pattern_into(stream_builder, device_exchange_probes.TensorOutlivesHandleProbe)


@graph_generator
def a_test_pattern_into_host_side_probe(stream_builder: StreamBuilder) -> None:
    _add_a_test_pattern_into(stream_builder, device_exchange_probes.HostSideProbe)


@graph_generator
def a_test_pattern_into_copy_request_refused_at_both_doors_probe(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into(
        stream_builder, device_exchange_probes.CopyRequestRefusedAtBothDoorsProbe
    )


@graph_generator
def a_test_pattern_into_pixel_buffer_scope_discards_on_raise_probe(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into(
        stream_builder, device_exchange_probes.PixelBufferScopeDiscardsOnRaiseProbe
    )


@graph_generator
def a_test_pattern_into_mlx_reads_the_frame_probe(stream_builder: StreamBuilder) -> None:
    _add_a_test_pattern_into(stream_builder, device_exchange_probes.MlxReadsTheFrameProbe)


@graph_generator
def a_test_pattern_into_mlx_writes_the_frame_through_the_write_door_probe(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into(
        stream_builder, device_exchange_probes.MlxWritesTheFrameThroughTheWriteDoorProbe
    )


@graph_generator
def a_test_pattern_into_mlx_write_with_a_view_alive_misses_the_frame_probe(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into(
        stream_builder, device_exchange_probes.MlxWriteWithAViewAliveMissesTheFrameProbe
    )


@graph_generator
def a_test_pattern_into_mlx_whole_array_assignment_misses_the_frame_probe(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into(
        stream_builder, device_exchange_probes.MlxWholeArrayAssignmentMissesTheFrameProbe
    )


@graph_generator
def dma_buf_export_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.DmaBufExportProbe)


@graph_generator
def privileged_capability_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.PrivilegedCapabilityProbe)


@graph_generator
def texture_handle_round_trip_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.TextureHandleRoundTripProbe)


@graph_generator
def opaque_fd_export_handoff_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.OpaqueFdExportHandoffProbe)


@graph_generator
def opaque_fd_pixel_buffer_export_handoff_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.OpaqueFdPixelBufferExportHandoffProbe)


@graph_generator
def device_tensor_scope_doubles_a_kernel_output_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.DeviceTensorScopeDoublesAKernelOutputProbe)


@graph_generator
def device_tensor_scope_discards_on_raise_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.DeviceTensorScopeDiscardsOnRaiseProbe)


@graph_generator
def pooled_texture_export_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.PooledTextureExportProbe)


@graph_generator
def device_tensor_scope_takes_every_acquired_texture_probe_alone(
    stream_builder: StreamBuilder,
) -> None:
    stream_builder.add(device_exchange_probes.DeviceTensorScopeTakesEveryAcquiredTextureProbe)


@graph_generator
def device_tensor_strides_follow_the_row_pitch_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.DeviceTensorStridesFollowTheRowPitchProbe)


@graph_generator
def torch_device_write_then_engine_gpu_read_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.TorchDeviceWriteThenEngineGpuReadProbe)


@graph_generator
def mlx_device_write_then_engine_gpu_read_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.MlxDeviceWriteThenEngineGpuReadProbe)


@graph_generator
def torch_and_mlx_scopes_alternate_in_one_helper_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.TorchAndMlxScopesAlternateInOneHelperProbe)


@graph_generator
def torch_tensor_outlives_texture_handle_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.TorchTensorOutlivesTextureHandleProbe)


@graph_generator
def mlx_array_outlives_texture_handle_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.MlxArrayOutlivesTextureHandleProbe)


@graph_generator
def iosurface_export_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.IOSurfaceExportProbe)


@graph_generator
def raw_handle_off_its_platform_refuses_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(device_exchange_probes.RawHandleOffItsPlatformRefusesProbe)
