# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A StreamLib app: one camera in, two virtual cameras out.

`streamlib dev` finds `setup(rt)` below by convention — there is no manifest and
no `main()`. While this app runs, every other application on the machine sees
two extra cameras in its picker: one showing the capture device untouched, one
showing the same picture with its colors inverted by a Python processor.

Processors live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

import os

from processors.inverting_effect import InvertingEffect
from streamlib import CameraSource, Runtime, VirtualCameraSink

DEFAULT_PASSTHROUGH_CAMERA_NAME = "StreamLib Camera"
DEFAULT_INVERTED_CAMERA_NAME = "StreamLib Camera Inverted"


def _virtual_camera_configuration(camera_name: str) -> dict[str, object]:
    configuration: dict[str, object] = {"name": camera_name}
    # Unset leaves the sink on its own `auto`: the v4l2loopback door where the
    # module's control node is writable, the PipeWire door otherwise.
    requested_door = os.environ.get("STREAMLIB_VIRTUAL_CAMERA_DOOR")
    if requested_door:
        configuration["door"] = requested_door
    return configuration


def setup(rt: Runtime) -> None:
    camera_configuration: dict[str, object] = {}
    # Unset means "the first capture device the engine finds"; set it to point
    # this app at a particular node, a vivid virtual camera included.
    requested_camera_device = os.environ.get("STREAMLIB_CAMERA_DEVICE")
    if requested_camera_device:
        camera_configuration["device_id"] = requested_camera_device

    camera = rt.add(CameraSource, config=camera_configuration)
    inverting_effect = rt.add(InvertingEffect)
    passthrough_virtual_camera = rt.add(
        VirtualCameraSink,
        config=_virtual_camera_configuration(
            os.environ.get(
                "STREAMLIB_PASSTHROUGH_CAMERA_NAME", DEFAULT_PASSTHROUGH_CAMERA_NAME
            )
        ),
    )
    inverted_virtual_camera = rt.add(
        VirtualCameraSink,
        config=_virtual_camera_configuration(
            os.environ.get(
                "STREAMLIB_INVERTED_CAMERA_NAME", DEFAULT_INVERTED_CAMERA_NAME
            )
        ),
    )

    # One output port, two consumers: the camera's frames reach the passthrough
    # sink and the effect alike, and each sink is its own camera.
    rt.connect(camera.output("video"), passthrough_virtual_camera.input("video"))
    rt.connect(camera.output("video"), inverting_effect.input("video_from_upstream"))
    rt.connect(
        inverting_effect.output("video_to_downstream"),
        inverted_virtual_camera.input("video"),
    )
