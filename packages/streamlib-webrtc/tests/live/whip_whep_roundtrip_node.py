#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The graph the WebRTC live proof measures: WHIP out, WHEP back, in one node.

`CameraSource -> H264Encoder -> WhipPublisher` beside
`MicrophoneSource -> OpusEncoder -> WhipPublisher`, and
`WhepPlayer -> H264Decoder -> DisplayWindow` beside `-> OpusDecoder ->
SpeakerSink`. It is `examples/camera-webrtc-publish` with the playback half
attached: the same publish shape, with display names the driving script can
find processors by and a control plane it can read them through.

Nothing joins the two halves locally. Every frame the decoder publishes was
packetised into RTP, ingested by the endpoint, and depacketised back out of it,
so a channel mean taken off the decoder's output and matched against the vivid
baseline the codec rig captured is a statement about this wheel.

The two URLs arrive in the environment and never in argv: Cloudflare Stream
carries the stream key as a path segment, and argv is world-readable through
`/proc`. Neither is logged, printed, or written to the output directory.

`whip_whep_roundtrip.sh` drives it.
"""

import argparse
import os

import streamlib
from streamlib_webrtc import WhepPlayer, WhipPublisher

#: Stated rather than left to the encoder's default, because the baseline this
#: run locks against was captured with it stated: one baseline scores two paths
#: only if both present the same GOP structure to the decoder.
ENCODER_KEYFRAME_INTERVAL_SECONDS = 2

PUBLISH_URL_VARIABLE = "STREAMLIB_WHIP_URL"
PLAYBACK_URL_VARIABLE = "STREAMLIB_WHEP_URL"


def _url_from_the_environment(variable: str) -> str:
    url = os.environ.get(variable)
    if not url:
        raise SystemExit(
            f"{variable} is unset. Cloudflare Stream carries its key in the "
            f"URL's path, so the URL is the credential and there is no address "
            f"this fixture could default to. Absent credentials are a "
            f"cannot-run, not a failure."
        )
    return url


def _session_configuration(url_variable: str, token_variable: str) -> dict[str, object]:
    configuration: dict[str, object] = {"url": _url_from_the_environment(url_variable)}
    # Cloudflare Stream authenticates by the key in the path and needs none; an
    # endpoint wanting RFC 9725's `Authorization: Bearer` takes one.
    bearer_token = os.environ.get(token_variable)
    if bearer_token:
        configuration["bearer_token"] = bearer_token
    return configuration


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    # An argument and never an environment variable: a rig carrying both a
    # virtual and a real camera hands the first-enumerated node to a run that
    # does not name the one it means.
    parser.add_argument(
        "--camera",
        default=None,
        help="V4L2 node to capture from (default: the first the engine finds)",
    )
    parser.add_argument(
        "--audio-capture-device",
        default=None,
        help=(
            "audio device to capture from; the driver passes the fixture sink's "
            "monitor, so the known signal played into that sink is what crosses "
            "the network (default: the backend's own default device)"
        ),
    )
    parser.add_argument("--control-plane-port", type=int, default=9000)
    arguments = parser.parse_args()

    runtime = streamlib.Runtime()

    publisher = runtime.add(
        WhipPublisher,
        config=_session_configuration(
            PUBLISH_URL_VARIABLE, "STREAMLIB_WHIP_BEARER_TOKEN"
        ),
        display_name="publisher",
    )
    player = runtime.add(
        WhepPlayer,
        config=_session_configuration(
            PLAYBACK_URL_VARIABLE, "STREAMLIB_WHEP_BEARER_TOKEN"
        ),
        display_name="player",
    )

    camera = runtime.add(
        streamlib.CameraSource,
        config={"device_id": arguments.camera} if arguments.camera else {},
        display_name="camera",
    )
    video_encoder = runtime.add(
        streamlib.H264Encoder,
        config={"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS},
        display_name="video_encoder",
    )
    microphone = runtime.add(
        streamlib.MicrophoneSource,
        config=(
            {"device_id": arguments.audio_capture_device}
            if arguments.audio_capture_device
            else {}
        ),
        display_name="microphone",
    )
    audio_encoder = runtime.add(streamlib.OpusEncoder, display_name="audio_encoder")

    video_decoder = runtime.add(streamlib.H264Decoder, display_name="video_decoder")
    audio_decoder = runtime.add(streamlib.OpusDecoder, display_name="audio_decoder")
    # Both sinks are here so each decoder has a subscriber for the whole run,
    # which is the shape the showcase ships and the shape the codec rig scored.
    window = runtime.add(
        streamlib.DisplayWindow,
        config={"title": "streamlib whip/whep round-trip"},
        display_name="window",
    )
    speaker = runtime.add(streamlib.SpeakerSink, display_name="speaker")

    runtime.connect(camera.output("video"), video_encoder.input("video"))
    runtime.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
    runtime.connect(microphone.output("audio"), audio_encoder.input("audio"))
    runtime.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))

    runtime.connect(
        player.output("encoded_video"), video_decoder.input("encoded_video")
    )
    runtime.connect(video_decoder.output("video"), window.input("video"))
    runtime.connect(
        player.output("encoded_audio"), audio_decoder.input("encoded_audio")
    )
    runtime.connect(audio_decoder.output("audio"), speaker.input("audio"))

    # Loopback rather than the default every interface: this node exists to be
    # tapped from the machine it runs on, and it carries no authentication —
    # which matters more than usual here, because `graph` renders each
    # processor's config and this graph's config holds both endpoint URLs.
    runtime.host_control_plane(
        bind_host="127.0.0.1",
        bind_port=arguments.control_plane_port,
        node_name="whip-whep-roundtrip-node",
    )
    runtime.run()


if __name__ == "__main__":
    main()
