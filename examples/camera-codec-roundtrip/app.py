# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A StreamLib app: camera → hardware encoder → hardware decoder → window.

`streamlib run` finds `setup(rt)` below by convention — there is no manifest and
no `main()`. All four processors are native built-ins that ship inside the
wheel, so this app declares no processor of its own: the pipeline is four
`rt.add` calls and three `rt.connect` calls, and no frame on it ever enters a
Python interpreter.
"""

import os

from streamlib import (
    CameraSource,
    DisplayWindow,
    H264Decoder,
    H264Encoder,
    H265Decoder,
    H265Encoder,
    Runtime,
)

ENCODER_AND_DECODER_MARKERS_BY_CODEC: dict[str, tuple[type, type]] = {
    "h264": (H264Encoder, H264Decoder),
    "h265": (H265Encoder, H265Decoder),
}

DEFAULT_CODEC = "h264"


def _resolve_requested_codec() -> str:
    requested_codec = os.environ.get("STREAMLIB_CODEC", DEFAULT_CODEC)
    if requested_codec not in ENCODER_AND_DECODER_MARKERS_BY_CODEC:
        legal_codecs = ", ".join(sorted(ENCODER_AND_DECODER_MARKERS_BY_CODEC))
        raise ValueError(
            f"STREAMLIB_CODEC={requested_codec!r} names no codec this app can "
            f"build; it is one of: {legal_codecs}"
        )
    return requested_codec


def setup(rt: Runtime) -> None:
    encoder_marker, decoder_marker = ENCODER_AND_DECODER_MARKERS_BY_CODEC[
        _resolve_requested_codec()
    ]

    camera_configuration: dict[str, object] = {}
    # Unset means "the first capture device the engine finds"; set it to point
    # this app at a particular node, a vivid virtual camera included.
    requested_camera_device = os.environ.get("STREAMLIB_CAMERA_DEVICE")
    if requested_camera_device:
        camera_configuration["device_id"] = requested_camera_device

    camera = rt.add(CameraSource, config=camera_configuration)
    # Both blocks bare: every config key either one takes is optional, and the
    # encoder sizes its session from the first frame the camera hands it.
    encoder = rt.add(encoder_marker)
    decoder = rt.add(decoder_marker)
    window = rt.add(
        DisplayWindow,
        config={
            "title": "StreamLib Camera Codec Round-Trip",
            "width": 1920,
            "height": 1080,
            "scaling": "fit",
        },
    )

    rt.connect(camera.output("video"), encoder.input("video"))
    rt.connect(encoder.output("encoded_video"), decoder.input("encoded_video"))
    rt.connect(decoder.output("video"), window.input("video"))
