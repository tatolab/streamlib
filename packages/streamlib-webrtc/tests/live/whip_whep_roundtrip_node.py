#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The graph the WebRTC live proof measures: WHIP out, WHEP back, in one node.

`CameraSource -> H264Encoder -> WhipPublisher` beside
`MicrophoneSource -> OpusEncoder -> WhipPublisher`, and
`WhepPlayer -> H264Decoder -> DisplayWindow` beside `-> OpusDecoder ->
SpeakerSink`. It is `examples/camera-webrtc-publish` with the playback half
attached: the same publish shape, with node names the driving script can
find processors by and a local API socket it can read them through.

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
import functools
import os

import streamlib
from streamlib import Stream, compile_stream_to_graph, stream
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


@functools.cache
def _parse_fixture_arguments() -> argparse.Namespace:
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
    return parser.parse_args()


@stream
def whip_whep_roundtrip(stream: Stream) -> None:
    """The round trip this process's argv and endpoint URLs describe."""
    arguments = _parse_fixture_arguments()

    publisher = stream.add(
        WhipPublisher,
        config=_session_configuration(
            PUBLISH_URL_VARIABLE, "STREAMLIB_WHIP_BEARER_TOKEN"
        ),
        name="publisher",
    )
    player = stream.add(
        WhepPlayer,
        config=_session_configuration(
            PLAYBACK_URL_VARIABLE, "STREAMLIB_WHEP_BEARER_TOKEN"
        ),
        name="player",
    )

    camera = stream.add(
        streamlib.CameraSource,
        config={"device_id": arguments.camera} if arguments.camera else {},
        name="camera",
    )
    video_encoder = stream.add(
        streamlib.H264Encoder,
        config={"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS},
        name="video_encoder",
    )
    microphone = stream.add(
        streamlib.MicrophoneSource,
        config=(
            {"device_id": arguments.audio_capture_device}
            if arguments.audio_capture_device
            else {}
        ),
        name="microphone",
    )
    audio_encoder = stream.add(streamlib.OpusEncoder, name="audio_encoder")

    video_decoder = stream.add(streamlib.H264Decoder, name="video_decoder")
    audio_decoder = stream.add(streamlib.OpusDecoder, name="audio_decoder")
    # Both sinks are here so each decoder has a subscriber for the whole run,
    # which is the shape the showcase ships and the shape the codec rig scored.
    window = stream.add(
        streamlib.DisplayWindow,
        config={"title": "streamlib whip/whep round-trip"},
        name="window",
    )
    speaker = stream.add(streamlib.SpeakerSink, name="speaker")

    stream.connect(camera.output("video"), video_encoder.input("video"))
    stream.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
    stream.connect(microphone.output("audio"), audio_encoder.input("audio"))
    stream.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))

    stream.connect(
        player.output("encoded_video"), video_decoder.input("encoded_video")
    )
    stream.connect(video_decoder.output("video"), window.input("video"))
    stream.connect(
        player.output("encoded_audio"), audio_decoder.input("encoded_audio")
    )
    stream.connect(audio_decoder.output("audio"), speaker.input("audio"))


def main() -> None:
    _parse_fixture_arguments()
    graph = compile_stream_to_graph(whip_whep_roundtrip)

    runtime = streamlib.Runtime(runtime_name="whip-whep-roundtrip-node")
    runtime.load(graph)

    runtime.host_control_plane()
    runtime.run()


if __name__ == "__main__":
    main()
