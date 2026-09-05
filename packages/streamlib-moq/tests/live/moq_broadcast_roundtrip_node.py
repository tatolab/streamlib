#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The graph the MoQ live proof measures: out to a relay and back, in one node.

`CameraSource -> H264Encoder -> MoqBroadcastPublisher` beside
`MicrophoneSource -> OpusEncoder -> MoqBroadcastPublisher`, and
`MoqBroadcastSubscriber -> H264Decoder -> DisplayWindow` beside
`-> OpusDecoder -> SpeakerSink`. It is `examples/moq-broadcast-roundtrip`
measured: the same shape, with display names the driving script can find
processors by and a control plane it can read them through.

Nothing joins the two halves locally. Every frame the decoder publishes crossed
the network twice, so a channel mean taken off its output and matched against
the vivid baseline the codec rig captured is a statement about this wheel — the
rest of that path was already scored.

The relay URL arrives in the environment and never in argv: a draft-16 relay
carries its authentication token in the URL's path, and argv is world-readable
through `/proc`. It is never logged, printed, or written to the output
directory either.

`moq_broadcast_roundtrip.sh` drives it.
"""

import argparse
import os

import streamlib
from streamlib_moq import MoqBroadcastPublisher, MoqBroadcastSubscriber

#: What the subscriber asks for. Under CMAF the container names media tracks
#: `{track_id}.m4s`, numbered from one in declaration order — which is
#: `runtime.connect` order, so wiring video into `tracks` first is what makes
#: video track one.
VIDEO_TRACK_NAME = "1.m4s"
AUDIO_TRACK_NAME = "2.m4s"

#: Stated rather than left to the encoder's default, because the baseline this
#: run locks against was captured with it stated: one baseline scores two paths
#: only if both present the same GOP structure to the decoder.
ENCODER_KEYFRAME_INTERVAL_SECONDS = 2

#: The environment variable carrying the relay, token included.
RELAY_URL_VARIABLE = "STREAMLIB_MOQ_RELAY_URL"


def _relay_url_from_the_environment() -> str:
    relay_url = os.environ.get(RELAY_URL_VARIABLE)
    if not relay_url:
        raise SystemExit(
            f"{RELAY_URL_VARIABLE} is unset. A draft-16 relay is provisioned "
            f"per account and carries its token in the URL path, so there is no "
            f"address this fixture could default to. Absent credentials are a "
            f"cannot-run, not a failure."
        )
    return relay_url


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
        "--broadcast",
        required=True,
        help="the broadcast name both halves agree on",
    )
    parser.add_argument(
        "--container-format",
        choices=("cmaf", "streamlib_bag"),
        default="cmaf",
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

    relay_url = _relay_url_from_the_environment()

    runtime = streamlib.Runtime()

    publisher = runtime.add(
        MoqBroadcastPublisher,
        config={
            "relay_url": relay_url,
            "broadcast": arguments.broadcast,
            "container_format": arguments.container_format,
        },
        display_name="publisher",
    )
    # Under `streamlib_bag` a track is named after its link's own channel — a
    # cuid2 minted at add time, which nothing can be told in advance. The arm
    # that exercises that container names its tracks from the live graph
    # instead; here the CMAF fallback contract makes them knowable.
    subscriber_configuration: dict[str, object] = {
        "relay_url": relay_url,
        "broadcast": arguments.broadcast,
        "container_format": arguments.container_format,
    }
    if arguments.container_format == "cmaf":
        subscriber_configuration["video_track"] = VIDEO_TRACK_NAME
        subscriber_configuration["audio_track"] = AUDIO_TRACK_NAME
    else:
        subscriber_configuration["video_track"] = os.environ[
            "STREAMLIB_MOQ_VIDEO_TRACK"
        ]
        subscriber_configuration["audio_track"] = os.environ[
            "STREAMLIB_MOQ_AUDIO_TRACK"
        ]
    subscriber = runtime.add(
        MoqBroadcastSubscriber,
        config=subscriber_configuration,
        display_name="subscriber",
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
        config={"title": "streamlib moq broadcast round-trip"},
        display_name="window",
    )
    speaker = runtime.add(streamlib.SpeakerSink, display_name="speaker")

    # Video into `tracks` first: declaration order is track-id order under CMAF.
    runtime.connect(camera.output("video"), video_encoder.input("video"))
    runtime.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
    runtime.connect(microphone.output("audio"), audio_encoder.input("audio"))
    runtime.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))

    runtime.connect(
        subscriber.output("encoded_video"), video_decoder.input("encoded_video")
    )
    runtime.connect(video_decoder.output("video"), window.input("video"))
    runtime.connect(
        subscriber.output("encoded_audio"), audio_decoder.input("encoded_audio")
    )
    runtime.connect(audio_decoder.output("audio"), speaker.input("audio"))

    # Loopback rather than the default every interface: this node exists to be
    # tapped from the machine it runs on, and it carries no authentication —
    # which matters more than usual here, because `graph` renders each
    # processor's config and this graph's config holds the relay token.
    runtime.host_control_plane(
        bind_host="127.0.0.1",
        bind_port=arguments.control_plane_port,
        node_name="moq-broadcast-roundtrip-node",
    )
    runtime.run()


if __name__ == "__main__":
    main()
