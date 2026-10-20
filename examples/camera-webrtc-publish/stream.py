# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: camera + microphone → H.264 and Opus → one WHIP session.

`tatolab dev` loads the one `@stream` below by convention — there is no
manifest. Everything upstream of the publisher is a native built-in, so no frame
and no sample enters a Python interpreter; `WhipPublisher` comes from the
`tatolab-webrtc` package and runs in its own child interpreter.

The WHIP URL is a credential, so it is read from the environment and never
written here. The runtime compiles this file, so the environment is
`tatolabd`'s, not the shell that typed `tatolab dev`.
"""

import os

from tatolab.stream import (
    CameraSource,
    CameraSourceConfig,
    H264Encoder,
    MicrophoneSource,
    OpusEncoder,
    StreamBuilder,
    stream,
)
from tatolab.webrtc import WhipPublisher

# The capture device: a V4L2 path (`/dev/video2`) on Linux, an AVFoundation
# unique ID on macOS. `None` opens the first capture device found.
CAMERA_DEVICE_ID: str | None = None

WHIP_URL_ENVIRONMENT_VARIABLE = "STREAMLIB_WHIP_URL"
WHIP_BEARER_TOKEN_ENVIRONMENT_VARIABLE = "STREAMLIB_WHIP_BEARER_TOKEN"


def whip_publisher_config_from_the_runtime_environment() -> dict[str, str]:
    """The publisher's `url`, and its `bearer_token` when one is set, from `tatolabd`'s environment."""
    whip_url = os.environ.get(WHIP_URL_ENVIRONMENT_VARIABLE)
    if not whip_url:
        raise SystemExit(
            f"{WHIP_URL_ENVIRONMENT_VARIABLE} is unset in the runtime's environment. "
            f"It is the WHIP endpoint to publish to — for Cloudflare Stream, the "
            f"`.../webRTC/publish` URL its dashboard shows for a live input — and a "
            f"credential: set it where tatolabd starts, never in this file."
        )
    publisher_config = {"url": whip_url}
    # Cloudflare Stream authenticates by the key in the URL's path and needs no
    # token; an endpoint that wants RFC 9725's `Authorization: Bearer` takes one.
    bearer_token = os.environ.get(WHIP_BEARER_TOKEN_ENVIRONMENT_VARIABLE)
    if bearer_token:
        publisher_config["bearer_token"] = bearer_token
    return publisher_config


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The camera and the microphone, encoded and published as one WHIP session."""
    camera_config: CameraSourceConfig = {}
    if CAMERA_DEVICE_ID is not None:
        camera_config["device_id"] = CAMERA_DEVICE_ID
    camera = stream_builder.add(CameraSource, config=camera_config)
    # Both encoders size themselves from what reaches them — the video session
    # from the first frame's extent, libopus from the first window's channels.
    video_encoder = stream_builder.add(H264Encoder)
    microphone = stream_builder.add(MicrophoneSource)
    audio_encoder = stream_builder.add(OpusEncoder)
    publisher = stream_builder.add(
        WhipPublisher, config=whip_publisher_config_from_the_runtime_environment()
    )

    stream_builder.connect(camera.output("video"), video_encoder.input("video"))
    stream_builder.connect(
        video_encoder.output("encoded_video"), publisher.input("tracks")
    )
    stream_builder.connect(microphone.output("audio"), audio_encoder.input("audio"))
    stream_builder.connect(
        audio_encoder.output("encoded_audio"), publisher.input("tracks")
    )
