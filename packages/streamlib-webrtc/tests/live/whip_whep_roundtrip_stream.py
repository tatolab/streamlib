# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The stream the WebRTC live proof measures: WHIP out, WHEP back, in one runtime.

`CameraSource -> H264Encoder -> WhipPublisher` beside
`MicrophoneSource -> OpusEncoder -> WhipPublisher`, and
`WhepPlayer -> H264Decoder -> DisplayWindow` beside `-> OpusDecoder ->
SpeakerSink`. It is `examples/camera-webrtc-publish` with the playback half
attached: the same publish shape, with node names the driving script can
find nodes by and a local API socket it can read them through.

Nothing joins the two halves locally. Every frame the decoder publishes was
packetised into RTP, ingested by the endpoint, and depacketised back out of it,
so a channel mean taken off the decoder's output and matched against the vivid
baseline the codec rig captured is a statement about this wheel.

The two URLs arrive in the environment and never in argv: Cloudflare Stream
carries the stream key as a path segment, and argv is world-readable through
`/proc`. Neither is logged, printed, or written to the output directory.

`whip_whep_roundtrip.sh` runs it with `tatolab run`, which hands a stream no
argv, so the devices arrive in the environment too: `STREAMLIB_CAMERA_DEVICE`
names the V4L2 node (else the first the engine finds), and
`STREAMLIB_AUDIO_CAPTURE_DEVICE_ID` the audio device to capture from — the
driver passes the fixture sink's monitor, so the known signal played into that
sink is what crosses the network (else the backend's own default device).
"""

import os

import tatolab.stream
from tatolab.webrtc import WhepPlayer, WhipPublisher
from tatolab.stream import StreamBuilder, stream

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


@stream
def whip_whep_roundtrip(stream_builder: StreamBuilder) -> None:
    """The round trip this process's environment describes."""
    camera_device = os.environ.get("STREAMLIB_CAMERA_DEVICE")
    audio_capture_device = os.environ.get("STREAMLIB_AUDIO_CAPTURE_DEVICE_ID")

    publisher = stream_builder.add(
        WhipPublisher,
        config=_session_configuration(
            PUBLISH_URL_VARIABLE, "STREAMLIB_WHIP_BEARER_TOKEN"
        ),
        name="publisher",
    )
    player = stream_builder.add(
        WhepPlayer,
        config=_session_configuration(
            PLAYBACK_URL_VARIABLE, "STREAMLIB_WHEP_BEARER_TOKEN"
        ),
        name="player",
    )

    camera = stream_builder.add(
        tatolab.stream.CameraSource,
        config={"device_id": camera_device} if camera_device else {},
        name="camera",
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

    video_decoder = stream_builder.add(tatolab.stream.H264Decoder, name="video_decoder")
    audio_decoder = stream_builder.add(tatolab.stream.OpusDecoder, name="audio_decoder")
    # Both sinks are here so each decoder has a subscriber for the whole run,
    # which is the shape the showcase ships and the shape the codec rig scored.
    window = stream_builder.add(
        tatolab.stream.DisplayWindow,
        config={"title": "streamlib whip/whep round-trip"},
        name="window",
    )
    speaker = stream_builder.add(tatolab.stream.SpeakerSink, name="speaker")

    stream_builder.connect(camera.output("video"), video_encoder.input("video"))
    stream_builder.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
    stream_builder.connect(microphone.output("audio"), audio_encoder.input("audio"))
    stream_builder.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))

    stream_builder.connect(
        player.output("encoded_video"), video_decoder.input("encoded_video")
    )
    stream_builder.connect(video_decoder.output("video"), window.input("video"))
    stream_builder.connect(
        player.output("encoded_audio"), audio_decoder.input("encoded_audio")
    )
    stream_builder.connect(audio_decoder.output("audio"), speaker.input("audio"))

