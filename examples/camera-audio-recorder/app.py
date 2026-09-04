# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A StreamLib app: camera + microphone → one MP4, two tracks.

`streamlib run` finds `setup(rt)` below by convention — there is no manifest and
no `main()`. All five processors are native built-ins that ship inside the
wheel, so this app declares no processor of its own, and neither a frame nor a
sample ever enters a Python interpreter.

Nothing here configures the recording's track layout: both encoders are wired
into the sink's one `tracks` input, and `Mp4Sink` makes one track per inbound
link. Ctrl-C stops the pipeline, and the sink's teardown closes the file.
"""

import os

from streamlib import (
    CameraSource,
    DisplayWindow,
    H264Encoder,
    MicrophoneSource,
    Mp4Sink,
    OpusEncoder,
    Runtime,
)

DEFAULT_RECORDING_PATH = "recording.mp4"


def setup(rt: Runtime) -> None:
    camera_configuration: dict[str, object] = {}
    # Unset means "the first capture device the engine finds"; set it to point
    # this app at a particular node.
    requested_camera_device = os.environ.get("STREAMLIB_CAMERA_DEVICE")
    if requested_camera_device:
        camera_configuration["device_id"] = requested_camera_device

    recorder = rt.add(
        Mp4Sink,
        config={
            "path": os.environ.get(
                "STREAMLIB_RECORDING_PATH", DEFAULT_RECORDING_PATH
            )
        },
    )

    camera = rt.add(CameraSource, config=camera_configuration)
    # Bare: every config key the encoder takes is optional, and it sizes its
    # session from the first frame the camera hands it.
    video_encoder = rt.add(H264Encoder)
    window = rt.add(
        DisplayWindow,
        config={
            "title": "StreamLib Camera Audio Recorder",
            "width": 1920,
            "height": 1080,
            "scaling": "fit",
        },
    )

    # The microphone's own rate and channel count reach the encoder unconverted:
    # `OpusEncoder` declares a window contract naming no channel count, so the
    # engine resamples and frames to Opus's clock and the count follows the
    # device.
    microphone = rt.add(MicrophoneSource)
    audio_encoder = rt.add(OpusEncoder)

    rt.connect(camera.output("video"), video_encoder.input("video"))
    rt.connect(video_encoder.output("encoded_video"), recorder.input("tracks"))
    # The same camera output feeds the window: a preview of what is being
    # recorded, not a second capture.
    rt.connect(camera.output("video"), window.input("video"))

    rt.connect(microphone.output("audio"), audio_encoder.input("audio"))
    rt.connect(audio_encoder.output("encoded_audio"), recorder.input("tracks"))
