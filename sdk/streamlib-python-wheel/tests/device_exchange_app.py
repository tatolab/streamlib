# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run one device-exchange probe in its real placement.

Run as a real `python app.py`: the probe executes in a helper process, reaches
the frame's pixels as CUDA memory from there, and its observation reaches this
app — and the test driving it — over the child→parent log forwarding.
"""

import sys

import streamlib

import device_exchange_probes


def scenario_frame_probe(probe_class_name: str) -> None:
    """A native test pattern into one probe: the probe reports on its first
    frame, and the graph runs until the test has read the result."""
    runtime = streamlib.Runtime()
    pattern = runtime.add(
        streamlib.TestPatternSource,
        config={
            "width": device_exchange_probes.SURFACE_WIDTH,
            "height": device_exchange_probes.SURFACE_HEIGHT,
        },
    )
    probe = runtime.add(getattr(device_exchange_probes, probe_class_name))
    runtime.connect(pattern.output("video"), probe.input("video_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


def scenario_camera_probe() -> None:
    """The camera's ring re-registers a different texture under one surface id
    every frame, and its pool recycles a slot every few frames — the two ways
    the pixels under a published id used to change underneath a reader."""
    runtime = streamlib.Runtime()
    camera = runtime.add(streamlib.CameraSource, config={"device_id": "/dev/video0"})
    probe = runtime.add(device_exchange_probes.LaggedConsumerHoldsItsFrameProbe)
    runtime.connect(camera.output("video"), probe.input("video_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


def scenario_standalone_probe(probe_class_name: str) -> None:
    """A probe that needs no upstream: it reports from `setup`."""
    runtime = streamlib.Runtime()
    runtime.add(getattr(device_exchange_probes, probe_class_name))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    scenario = sys.argv[1]
    if scenario == "camera":
        scenario_camera_probe()
    elif scenario in (
        "DmaBufExportProbe",
        "PrivilegedCapabilityProbe",
        "TextureHandleRoundTripProbe",
    ):
        scenario_standalone_probe(scenario)
    else:
        scenario_frame_probe(scenario)
