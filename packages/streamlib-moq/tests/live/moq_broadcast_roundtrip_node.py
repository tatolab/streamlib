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

`--container-format streamlib_bag` runs the same graph with a third track: a
`TelemetryBagSource` publishing one small bag per tick into `tracks`, and the
subscriber's `data_bags` read by a `TelemetryBagSink` on the far side. The
tracks are named rather than numbered there — `track_names` is what lets a
subscriber ask for `video`, `audio` and `telemetry` — and the data arm's
verdict is `verify_tapped_telemetry_bags.py` over what came back.

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
from typing import Any

import streamlib
from moq_live_telemetry_processors import TelemetryBagSink, TelemetryBagSource
from streamlib_moq import MoqBroadcastPublisher, MoqBroadcastSubscriber

#: Both containers this wheel writes, each its own arm of the proof. `cmaf` is
#: the one `moq-sub` reads, so it is what the interop arm needs; `streamlib_bag`
#: is the one a data track rides, and the only one whose track names the app
#: chooses.
CONTAINER_FORMATS = ("cmaf", "streamlib_bag")

#: What the subscriber asks for under CMAF: the container names media tracks
#: `{track_id}.m4s`, numbered from one in declaration order — which is
#: `runtime.connect` order, so wiring video into `tracks` first is what makes
#: video track one.
CMAF_VIDEO_TRACK_NAME = "1.m4s"
CMAF_AUDIO_TRACK_NAME = "2.m4s"

#: What the subscriber asks for under `streamlib_bag`, and what the publisher's
#: `track_names` declares them as — positionally, in wiring order, so this
#: tuple's order is the order the connects below run in.
BAG_VIDEO_TRACK_NAME = "video"
BAG_AUDIO_TRACK_NAME = "audio"
BAG_DATA_TRACK_NAME = "telemetry"

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
    parser.add_argument(
        "--container-format",
        choices=CONTAINER_FORMATS,
        default="cmaf",
        help=(
            "which container the broadcast is written in; `streamlib_bag` adds "
            "the named tracks and the telemetry data track"
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
    carries_a_data_track = arguments.container_format == "streamlib_bag"

    runtime = streamlib.Runtime()

    publisher_config: "dict[str, Any]" = {
        "relay_url": relay_url,
        "broadcast": arguments.broadcast,
        "container_format": arguments.container_format,
        "delivery_deadline_ms": arguments.delivery_deadline_ms,
    }
    subscriber_config: "dict[str, Any]" = {
        "relay_url": relay_url,
        "broadcast": arguments.broadcast,
        "container_format": arguments.container_format,
        "video_track": CMAF_VIDEO_TRACK_NAME,
        "audio_track": None if arguments.video_only else CMAF_AUDIO_TRACK_NAME,
    }
    if carries_a_data_track:
        # Positional, in the order the connects below run: video, then audio
        # where there is any, then telemetry. Under CMAF the names are the
        # interop contract and the publisher refuses to be given them.
        publisher_config["track_names"] = [
            BAG_VIDEO_TRACK_NAME,
            *([] if arguments.video_only else [BAG_AUDIO_TRACK_NAME]),
            BAG_DATA_TRACK_NAME,
        ]
        subscriber_config["video_track"] = BAG_VIDEO_TRACK_NAME
        subscriber_config["audio_track"] = (
            None if arguments.video_only else BAG_AUDIO_TRACK_NAME
        )
        subscriber_config["data_track"] = BAG_DATA_TRACK_NAME

    publisher = runtime.add(
        MoqBroadcastPublisher, config=publisher_config, display_name="publisher"
    )
    subscriber = runtime.add(
        MoqBroadcastSubscriber, config=subscriber_config, display_name="subscriber"
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

    if carries_a_data_track:
        telemetry_source = runtime.add(
            TelemetryBagSource, display_name="telemetry_source"
        )
        telemetry_sink = runtime.add(TelemetryBagSink, display_name="telemetry_sink")
        # Last into `tracks`, so `track_names` names it last.
        runtime.connect(
            telemetry_source.output("telemetry"), publisher.input("tracks")
        )
        runtime.connect(
            subscriber.output("data_bags"), telemetry_sink.input("data_bags")
        )

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
