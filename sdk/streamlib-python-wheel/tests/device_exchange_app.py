# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run one device-exchange probe in its real placement.

Run as its own `python <script>.py` process: the probe executes in a helper
process, reaches the frame's pixels as device memory from there, and its
observation reaches this app — and the test driving it — over the child→parent
log forwarding.
"""

import sys

import tatolab.runtime
import tatolab.stream
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream

import device_exchange_probes
from camera_under_test import camera_source_config


def _probe_class_named_on_the_command_line() -> type:
    return getattr(device_exchange_probes, sys.argv[1])


@stream
def a_test_pattern_into_one_frame_probe(stream_builder: StreamBuilder) -> None:
    """A native test pattern into one probe: the probe reports on its first
    frame, and the graph runs until the test has read the result."""
    pattern = stream_builder.add(
        tatolab.stream.TestPatternSource,
        config={
            "width": device_exchange_probes.SURFACE_WIDTH,
            "height": device_exchange_probes.SURFACE_HEIGHT,
        },
    )
    probe = stream_builder.add(_probe_class_named_on_the_command_line())
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
def one_standalone_device_exchange_probe(stream_builder: StreamBuilder) -> None:
    """A probe that needs no upstream: it reports from `setup`."""
    stream_builder.add(_probe_class_named_on_the_command_line())


if __name__ == "__main__":
    scenario = sys.argv[1]
    if scenario == "camera":
        stream_function = camera_into_the_lagged_consumer_probe
    elif scenario in (
        "DmaBufExportProbe",
        "PrivilegedCapabilityProbe",
        "TextureHandleRoundTripProbe",
        "OpaqueFdExportHandoffProbe",
        "DeviceTensorScopeDoublesAKernelOutputProbe",
        "DeviceTensorScopeDiscardsOnRaiseProbe",
        "PooledTextureExportProbe",
        "DeviceTensorScopeTakesEveryAcquiredTextureProbe",
        "DeviceTensorStridesFollowTheRowPitchProbe",
        "TorchDeviceWriteThenEngineGpuReadProbe",
        "MlxDeviceWriteThenEngineGpuReadProbe",
        "TorchAndMlxScopesAlternateInOneHelperProbe",
        "TorchTensorOutlivesTextureHandleProbe",
        "MlxArrayOutlivesTextureHandleProbe",
        "IOSurfaceExportProbe",
        "RawHandleOffItsPlatformRefusesProbe",
    ):
        stream_function = one_standalone_device_exchange_probe
    else:
        stream_function = a_test_pattern_into_one_frame_probe
    graph = compile_stream_to_graph(stream_function)
    runtime = tatolab.runtime.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
