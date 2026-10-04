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

The node names are for reading a run: they are what this node's own log
lines and `streamlib graph` show. Nothing downstream keys on them — a track is
named by the channel its link subscribed to, which carries the engine-minted
processor id, so `e2e_fixture_recording.sh` checks the recorded track names by
their `/encoded_video` and `/encoded_audio` suffixes instead.
"""

import argparse

import streamlib
from known_audio_signal_source import KnownAudioSignalSource
from streamlib import Stream, compile_stream_to_graph, stream

_VIDEO_ENCODER_MARKERS_BY_CODEC: dict[str, type] = {
    "h264": streamlib.H264Encoder,
    "h265": streamlib.H265Encoder,
}

# Stated rather than left to the encoder's own default, because the fragment
# rule follows it: with a video track wired, `Mp4Sink` closes a fragment at
# that track's sync points, so this is also how often the recording becomes
# playable a little further.
ENCODER_KEYFRAME_INTERVAL_SECONDS = 2


def _parse_fixture_arguments() -> argparse.Namespace:
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
    return parser.parse_args()


@stream
def camera_and_known_signal_recorded_into_one_file(stream: Stream) -> None:
    arguments = _parse_fixture_arguments()

    recorder = stream.add(
        streamlib.Mp4Sink,
        name="recorder",
        config={"path": arguments.path},
    )

    camera = stream.add(
        streamlib.CameraSource,
        name="camera",
        config={"device_id": arguments.camera} if arguments.camera else {},
    )
    video_encoder = stream.add(
        _VIDEO_ENCODER_MARKERS_BY_CODEC[arguments.codec],
        name="video_encoder",
        config={"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS},
    )
    stream.connect(camera.output("video"), video_encoder.input("video"))
    stream.connect(video_encoder.output("encoded_video"), recorder.input("tracks"))

    signal = stream.add(KnownAudioSignalSource, name="known_signal")
    audio_encoder = stream.add(streamlib.OpusEncoder, name="audio_encoder")
    stream.connect(signal.output("audio"), audio_encoder.input("audio"))
    stream.connect(audio_encoder.output("encoded_audio"), recorder.input("tracks"))


def main() -> None:
    arguments = _parse_fixture_arguments()
    graph = compile_stream_to_graph(camera_and_known_signal_recorded_into_one_file)
    runtime = streamlib.Runtime(runtime_name="recording-node")
    runtime.load(graph)

    # Loopback rather than the default every interface: this node exists to be
    # watched from the machine it runs on, and it carries no authentication.
    runtime.host_control_plane(
        bind_host="127.0.0.1",
        bind_port=arguments.control_plane_port,
    )
    runtime.run()


if __name__ == "__main__":
    main()
