# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A StreamLib app: camera + microphone → one MP4, two tracks, with a preview.

`streamlib run` finds `setup(rt)` below by convention — there is no manifest and
no `main()`. All six processors are native built-ins that ship inside the wheel,
so this app declares no processor of its own, and neither a frame nor a sample
ever enters a Python interpreter.

Nothing here configures the recording's track layout: both encoders are wired
into the sink's one `tracks` input, and `Mp4Sink` makes one track per inbound
link. Ctrl-C stops the pipeline, and the sink's teardown closes the file.

The preview hangs off the *encoder*, not the camera, and decodes the stream back
for the window. That is a wiring constraint made into the honest picture: a
channel's one publisher shares a single ring config with every subscriber, so a
source port cannot feed an `ordered` destination and a `newest` one at once —
and `H264Encoder`'s input is `ordered` while `DisplayWindow`'s is `newest`. Both
destinations of `encoded_video` are `ordered`, so this fan-out is legal, and what
reaches the glass is the bitstream that reached the file.
"""

import os

from streamlib import (
    CameraSource,
    DisplayWindow,
    H264Decoder,
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
    # Bare, all three: every config key the codec blocks take is optional. The
    # encoder sizes its session from the first frame the camera hands it, and
    # the decoder auto-detects its buffer size from the stream's first SPS.
    video_encoder = rt.add(H264Encoder)
    preview_decoder = rt.add(H264Decoder)
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
    rt.connect(
        video_encoder.output("encoded_video"),
        preview_decoder.input("encoded_video"),
    )
    rt.connect(preview_decoder.output("video"), window.input("video"))

    rt.connect(microphone.output("audio"), audio_encoder.input("audio"))
    rt.connect(audio_encoder.output("encoded_audio"), recorder.input("tracks"))
