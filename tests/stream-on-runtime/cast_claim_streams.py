# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The streams `test_cast_claim.py` starts on `tatolabd`: one probe off one real source.

The camera is what the lifetime probes need — only a real capture pool
recycles a slot underneath a held frame. The native test pattern serves the
probes that only need a real surface published by a real producer, so they run
on any GPU rather than only on a rig with a camera.
"""

import tatolab.stream
from tatolab.stream import NodeReference, StreamBuilder, stream

import cast_claim_probes
from camera_under_test import camera_source_config


def _add_the_camera(stream_builder: StreamBuilder) -> NodeReference:
    return stream_builder.add(tatolab.stream.CameraSource, config=camera_source_config())


def _add_the_test_pattern(stream_builder: StreamBuilder) -> NodeReference:
    return stream_builder.add(
        tatolab.stream.TestPatternSource, config={"width": 640, "height": 480}
    )


def _add_the_probe_off(
    stream_builder: StreamBuilder, source: NodeReference, probe_class: type
) -> None:
    probe = stream_builder.add(probe_class)
    stream_builder.connect(source.output("video"), probe.input("video_from_upstream"))


@stream
def typed_cast_holds_its_frame_probe_off_the_camera(stream_builder: StreamBuilder) -> None:
    _add_the_probe_off(
        stream_builder,
        _add_the_camera(stream_builder),
        cast_claim_probes.TypedCastHoldsItsFrameProbe,
    )


@stream
def untyped_read_holds_nothing_probe_off_the_camera(stream_builder: StreamBuilder) -> None:
    _add_the_probe_off(
        stream_builder,
        _add_the_camera(stream_builder),
        cast_claim_probes.UntypedReadHoldsNothingProbe,
    )


@stream
def a_user_authored_cast_reaches_its_pixels_bare_probe_off_the_test_pattern(
    stream_builder: StreamBuilder,
) -> None:
    _add_the_probe_off(
        stream_builder,
        _add_the_test_pattern(stream_builder),
        cast_claim_probes.AUserAuthoredCastReachesItsPixelsBareProbe,
    )


@stream
def the_shipped_video_frame_reaches_its_pixels_bare_probe_off_the_test_pattern(
    stream_builder: StreamBuilder,
) -> None:
    _add_the_probe_off(
        stream_builder,
        _add_the_test_pattern(stream_builder),
        cast_claim_probes.TheShippedVideoFrameReachesItsPixelsBareProbe,
    )


@stream
def a_user_authored_cast_reaches_its_pixels_as_a_device_tensor_probe_off_the_camera(
    stream_builder: StreamBuilder,
) -> None:
    _add_the_probe_off(
        stream_builder,
        _add_the_camera(stream_builder),
        cast_claim_probes.AUserAuthoredCastReachesItsPixelsAsADeviceTensorProbe,
    )


@stream
def the_shipped_video_frame_reaches_its_pixels_as_a_device_tensor_probe_off_the_camera(
    stream_builder: StreamBuilder,
) -> None:
    _add_the_probe_off(
        stream_builder,
        _add_the_camera(stream_builder),
        cast_claim_probes.TheShippedVideoFrameReachesItsPixelsAsADeviceTensorProbe,
    )


@stream
def the_gpu_write_door_edits_the_frame_probe_off_the_test_pattern(
    stream_builder: StreamBuilder,
) -> None:
    _add_the_probe_off(
        stream_builder,
        _add_the_test_pattern(stream_builder),
        cast_claim_probes.TheGpuWriteDoorEditsTheFrameProbe,
    )


@stream
def the_gpu_write_door_edits_the_frame_probe_off_the_camera(stream_builder: StreamBuilder) -> None:
    _add_the_probe_off(
        stream_builder,
        _add_the_camera(stream_builder),
        cast_claim_probes.TheGpuWriteDoorEditsTheFrameProbe,
    )


@stream
def a_raise_inside_the_gpu_write_door_discards_the_edit_probe_off_the_test_pattern(
    stream_builder: StreamBuilder,
) -> None:
    _add_the_probe_off(
        stream_builder,
        _add_the_test_pattern(stream_builder),
        cast_claim_probes.ARaiseInsideTheGpuWriteDoorDiscardsTheEditProbe,
    )


@stream
def the_cpu_write_door_edits_the_frame_probe_off_the_test_pattern(
    stream_builder: StreamBuilder,
) -> None:
    _add_the_probe_off(
        stream_builder,
        _add_the_test_pattern(stream_builder),
        cast_claim_probes.TheCpuWriteDoorEditsTheFrameProbe,
    )


@stream
def the_cpu_write_door_edits_the_frame_probe_off_the_camera(stream_builder: StreamBuilder) -> None:
    _add_the_probe_off(
        stream_builder,
        _add_the_camera(stream_builder),
        cast_claim_probes.TheCpuWriteDoorEditsTheFrameProbe,
    )


@stream
def a_raise_inside_the_cpu_write_door_propagates_probe_off_the_test_pattern(
    stream_builder: StreamBuilder,
) -> None:
    _add_the_probe_off(
        stream_builder,
        _add_the_test_pattern(stream_builder),
        cast_claim_probes.ARaiseInsideTheCpuWriteDoorPropagatesProbe,
    )
