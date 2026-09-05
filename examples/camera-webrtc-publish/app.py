# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A StreamLib app: camera + microphone → WHIP, one RTP track each.

`streamlib run` finds `setup(rt)` below by convention — there is no manifest and
no `main()`. Everything upstream of the publisher is a native built-in, so no
frame and no sample enters a Python interpreter; `WhipPublisher` comes from the
`streamlib-webrtc` extension wheel and runs in its own helper process, where the
session's whole life is that wheel's Rust.

The publisher is `Mp4Sink`'s shape reused: one fan-in input, and each inbound
link becomes one track whose medium the link's first bag settles by its `codec`.
One WHIP session carries at most one video and one audio track, so a second link
of either medium is refused by name at `setup()` rather than silently dropped.
"""

import os

from streamlib import (
    CameraSource,
    H264Encoder,
    MicrophoneSource,
    OpusEncoder,
    Runtime,
)
from streamlib_webrtc import WhipPublisher


def _required_whip_url() -> str:
    """The endpoint, from the environment and never from this file.

    A WHIP URL is a credential: Cloudflare Stream carries the stream key as a
    path segment, so a URL committed here would be a published ingest key. The
    app refuses to start without one rather than falling back to a placeholder
    that would fail later and further away.
    """
    whip_url = os.environ.get("STREAMLIB_WHIP_URL")
    if not whip_url:
        raise ValueError(
            "STREAMLIB_WHIP_URL is unset. It is the WHIP endpoint to publish "
            "to — for Cloudflare Stream, the `.../webRTC/publish` URL its "
            "dashboard shows for a live input. It is a credential: export it, "
            "never commit it."
        )
    return whip_url


def setup(rt: Runtime) -> None:
    publisher_configuration: dict[str, object] = {"url": _required_whip_url()}
    # Cloudflare Stream authenticates by the key in the URL's path and needs
    # none; a WHIP endpoint that wants RFC 9725's `Authorization: Bearer` takes
    # one here.
    bearer_token = os.environ.get("STREAMLIB_WHIP_BEARER_TOKEN")
    if bearer_token:
        publisher_configuration["bearer_token"] = bearer_token

    publisher = rt.add(WhipPublisher, config=publisher_configuration)

    camera_configuration: dict[str, object] = {}
    # Unset means "the first capture device the engine finds"; set it to point
    # this app at a particular node.
    requested_camera_device = os.environ.get("STREAMLIB_CAMERA_DEVICE")
    if requested_camera_device:
        camera_configuration["device_id"] = requested_camera_device

    camera = rt.add(CameraSource, config=camera_configuration)
    # Both encoders bare: every key either takes is optional, and each sizes
    # itself from what reaches it — the video session from the first frame's
    # extent, the libopus encoder from the first window's channel count.
    video_encoder = rt.add(H264Encoder)

    microphone = rt.add(MicrophoneSource)
    audio_encoder = rt.add(OpusEncoder)

    rt.connect(camera.output("video"), video_encoder.input("video"))
    rt.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))

    rt.connect(microphone.output("audio"), audio_encoder.input("audio"))
    rt.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))
