# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The WebRTC live proof's two streams: WHIP out of one, WHEP back into the other.

The sender encodes the test pattern and the microphone and publishes both to a
WHIP endpoint. The receiver plays the same live input back over WHEP and
decodes it. Nothing joins the two on this machine: every frame the receiver's
player writes was packetised into RTP, ingested by the endpoint, and
depacketised back out of it, so what it writes is a statement about this wheel.

The receiver exposes the player's `encoded_video` port, the frames that crossed
the network, which `whip_whep_roundtrip.sh` reads at the local relay with
`tatolab-read-track`, decodes with ffmpeg and scores; and the decoder's `video`
port, so `graph` counts what the decoder writes.

The two URLs arrive in the environment and never in argv: Cloudflare Stream
carries the stream key as a path segment, and argv is world-readable through
`/proc`. Neither is logged, printed, or written to the output directory.

`whip_whep_roundtrip.sh` loads each with `tatolab run`, which hands a stream no
argv, so the audio device arrives in the runtime's environment, which the
compile inherits: `STREAMLIB_AUDIO_CAPTURE_DEVICE_ID` names the device to
capture from — the driver passes the fixture sink's monitor, so the known
signal played into that sink is what crosses the network (else the backend's
own default device).
"""

import os

import tatolab.stream
from tatolab.webrtc import WhepPlayer, WhipPublisher
from tatolab.stream import StreamBuilder, stream

# Stated rather than left to the encoder's own default, so the baseline is
# measured over a GOP structure this file names.
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


@stream
def test_pattern_and_microphone_published_over_whip(stream_builder: StreamBuilder) -> None:
    audio_capture_device = os.environ.get("STREAMLIB_AUDIO_CAPTURE_DEVICE_ID")

    publisher = stream_builder.add(
        WhipPublisher,
        config=_session_configuration(
            PUBLISH_URL_VARIABLE, "STREAMLIB_WHIP_BEARER_TOKEN"
        ),
        name="publisher",
    )
    test_pattern = stream_builder.add(
        tatolab.stream.TestPatternSource, name="test_pattern"
    )
    video_encoder = stream_builder.add(
        tatolab.stream.H264Encoder,
        config={"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS},
        name="video_encoder",
    )
    microphone = stream_builder.add(
        tatolab.stream.MicrophoneSource,
        config={"device_id": audio_capture_device} if audio_capture_device else {},
        name="microphone",
    )
    audio_encoder = stream_builder.add(tatolab.stream.OpusEncoder, name="audio_encoder")

    stream_builder.connect(test_pattern.output("video"), video_encoder.input("video"))
    stream_builder.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
    stream_builder.connect(microphone.output("audio"), audio_encoder.input("audio"))
    stream_builder.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))


@stream
def the_same_input_played_back_over_whep_through_the_decoders(
    stream_builder: StreamBuilder,
) -> None:
    player = stream_builder.add(
        WhepPlayer,
        config=_session_configuration(
            PLAYBACK_URL_VARIABLE, "STREAMLIB_WHEP_BEARER_TOKEN"
        ),
        name="player",
    )
    video_decoder = stream_builder.add(tatolab.stream.H264Decoder, name="video_decoder")
    audio_decoder = stream_builder.add(tatolab.stream.OpusDecoder, name="audio_decoder")
    speaker = stream_builder.add(tatolab.stream.SpeakerSink, name="speaker")

    stream_builder.connect(
        player.output("encoded_video"), video_decoder.input("encoded_video")
    )
    stream_builder.connect(
        player.output("encoded_audio"), audio_decoder.input("encoded_audio")
    )
    stream_builder.connect(audio_decoder.output("audio"), speaker.input("audio"))

    stream_builder.expose(player.output("encoded_video"))
    stream_builder.expose(video_decoder.output("video"))
