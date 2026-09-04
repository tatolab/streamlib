#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A camera and the known signal recorded into one file, two tracks.

`CameraSource -> <codec>Encoder -> Mp4Sink` and
`KnownAudioSignalSource -> OpusEncoder -> Mp4Sink`. Two producers into the
sink's one `tracks` input, so the file owes two tracks and nothing between
them is configured — the sink enumerates its inbound links at `setup()` and
names each track after the channel it subscribed to.

The twin of `codec_roundtrip_node.py`: same camera, same encoder, same
authoring surface, with the container where the decoder was. That is what
makes the decode-back a real comparison — `e2e_fixture_recording.sh` replays
this file's video track back through the same decoder and locks it to the
same vivid baseline the live path locks to, with one file in between.

No display and no audio device. The known signal is generated rather than
captured for the same reason `opus_roundtrip_node.py` generates it: what is
being measured is the engine, not the rig's sound card. The signal runs for
its own length and then stops, which is a legal recording — a `moof` owes a
`traf` to no track — so the audio track is shorter than the video one by
design.

The display names are for reading a run: they are what this node's own log
lines and `streamlib graph` show. Nothing downstream keys on them — a track is
named by the channel its link subscribed to, which carries the engine-minted
processor id, so `e2e_fixture_recording.sh` checks the recorded track names by
their `/encoded_video` and `/encoded_audio` suffixes instead.
"""

import argparse

import streamlib
from known_audio_signal_source import KnownAudioSignalSource

_VIDEO_ENCODER_MARKERS_BY_CODEC: dict[str, type] = {
    "h264": streamlib.H264Encoder,
    "h265": streamlib.H265Encoder,
}

# Stated rather than left to the encoder's own default, because the fragment
# rule follows it: with a video track wired, `Mp4Sink` closes a fragment at
# that track's sync points, so this is also how often the recording becomes
# playable a little further.
ENCODER_KEYFRAME_INTERVAL_SECONDS = 2


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--codec",
        choices=sorted(_VIDEO_ENCODER_MARKERS_BY_CODEC),
        default="h264",
    )
    # An argument and never an environment variable: a rig carrying both a
    # virtual and a real camera hands the first-enumerated node to a run that
    # does not name the one it means.
    parser.add_argument(
        "--camera",
        default=None,
        help="V4L2 node to capture from (default: the first the engine finds)",
    )
    parser.add_argument(
        "--path",
        required=True,
        help="the file to record into, created or truncated at startup",
    )
    parser.add_argument("--control-plane-port", type=int, default=9000)
    arguments = parser.parse_args()

    runtime = streamlib.Runtime()
    recorder = runtime.add(
        streamlib.Mp4Sink,
        config={"path": arguments.path},
        display_name="recorder",
    )

    camera = runtime.add(
        streamlib.CameraSource,
        config={"device_id": arguments.camera} if arguments.camera else {},
        display_name="camera",
    )
    video_encoder = runtime.add(
        _VIDEO_ENCODER_MARKERS_BY_CODEC[arguments.codec],
        config={"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS},
        display_name="video_encoder",
    )
    runtime.connect(camera.output("video"), video_encoder.input("video"))
    runtime.connect(
        video_encoder.output("encoded_video"), recorder.input("tracks")
    )

    signal = runtime.add(KnownAudioSignalSource, display_name="known_signal")
    audio_encoder = runtime.add(streamlib.OpusEncoder, display_name="audio_encoder")
    runtime.connect(signal.output("audio"), audio_encoder.input("audio"))
    runtime.connect(
        audio_encoder.output("encoded_audio"), recorder.input("tracks")
    )

    # Loopback rather than the default every interface: this node exists to be
    # watched from the machine it runs on, and it carries no authentication.
    runtime.host_control_plane(
        bind_host="127.0.0.1",
        bind_port=arguments.control_plane_port,
        node_name="recording-node",
    )
    runtime.run()


if __name__ == "__main__":
    main()
