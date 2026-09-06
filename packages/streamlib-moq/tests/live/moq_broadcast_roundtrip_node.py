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

#: CMAF, because the `moq-sub` interop read is what this node proves. A
#: `streamlib_bag` run is no longer blocked on track naming — the publisher's
#: `track_names` lets a subscriber name what it wants — and that arm, with a
#: data track beside the media, is the fixture's next one; it is not built yet.
CONTAINER_FORMAT = "cmaf"

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
        "--audio-capture-device",
        default=None,
        help=(
            "audio device to capture from; the driver passes the fixture sink's "
            "monitor, so the known signal played into that sink is what crosses "
            "the network (default: the backend's own default device)"
        ),
    )
    parser.add_argument("--control-plane-port", type=int, default=9000)
    # The drop policy's two arms are one run each with this set and unset: the
    # baseline is as much of the deliverable as the improvement is, so the
    # fixture takes the deadline rather than hard-coding either arm.
    parser.add_argument(
        "--delivery-deadline-ms",
        type=int,
        default=None,
        help=(
            "how old a bag may be, by its own monotonic stamp, and still be "
            "published (default: no deadline, which publishes every bag)"
        ),
    )
    # The shaped-link arm's two knobs. Video-only because the CMAF init
    # segment waits on every declared track, and a silent microphone holds the
    # whole broadcast until the hold's byte bound stops it — a measurement of
    # the hold, not of the policy. A stated bitrate because constant-QP noise
    # is 2.3 MB a frame, which is a wall rather than congestion for any
    # ceiling a shaped link can set.
    parser.add_argument(
        "--video-only",
        action="store_true",
        help="publish the camera alone: no microphone, no audio track",
    )
    parser.add_argument(
        "--video-bitrate-bps",
        type=int,
        default=None,
        help="the H.264 encoder's target bitrate (default: constant QP)",
    )
    arguments = parser.parse_args()

    relay_url = _relay_url_from_the_environment()

    runtime = streamlib.Runtime()

    publisher = runtime.add(
        MoqBroadcastPublisher,
        config={
            "relay_url": relay_url,
            "broadcast": arguments.broadcast,
            "container_format": CONTAINER_FORMAT,
            "delivery_deadline_ms": arguments.delivery_deadline_ms,
        },
        display_name="publisher",
    )
    subscriber = runtime.add(
        MoqBroadcastSubscriber,
        config={
            "relay_url": relay_url,
            "broadcast": arguments.broadcast,
            "container_format": CONTAINER_FORMAT,
            "video_track": VIDEO_TRACK_NAME,
            "audio_track": None if arguments.video_only else AUDIO_TRACK_NAME,
        },
        display_name="subscriber",
    )

    camera = runtime.add(
        streamlib.CameraSource,
        config={"device_id": arguments.camera} if arguments.camera else {},
        display_name="camera",
    )
    video_encoder_config = {"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS}
    if arguments.video_bitrate_bps is not None:
        video_encoder_config["bitrate_bps"] = arguments.video_bitrate_bps
    video_encoder = runtime.add(
        streamlib.H264Encoder,
        config=video_encoder_config,
        display_name="video_encoder",
    )
    video_decoder = runtime.add(streamlib.H264Decoder, display_name="video_decoder")
    # A sink per decoder, so each has a subscriber for the whole run — the
    # shape the showcase ships and the shape the codec rig scored. The window
    # is here; the speaker sits with the audio path below.
    window = runtime.add(
        streamlib.DisplayWindow,
        config={"title": "streamlib moq broadcast round-trip"},
        display_name="window",
    )

    # Video into `tracks` first: declaration order is track-id order under CMAF.
    runtime.connect(camera.output("video"), video_encoder.input("video"))
    runtime.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
    runtime.connect(
        subscriber.output("encoded_video"), video_decoder.input("encoded_video")
    )
    runtime.connect(video_decoder.output("video"), window.input("video"))

    if not arguments.video_only:
        microphone = runtime.add(
            streamlib.MicrophoneSource,
            config=(
                {"device_id": arguments.audio_capture_device}
                if arguments.audio_capture_device
                else {}
            ),
            display_name="microphone",
        )
        audio_encoder = runtime.add(
            streamlib.OpusEncoder, display_name="audio_encoder"
        )
        audio_decoder = runtime.add(
            streamlib.OpusDecoder, display_name="audio_decoder"
        )
        speaker = runtime.add(streamlib.SpeakerSink, display_name="speaker")
        runtime.connect(microphone.output("audio"), audio_encoder.input("audio"))
        runtime.connect(
            audio_encoder.output("encoded_audio"), publisher.input("tracks")
        )
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
