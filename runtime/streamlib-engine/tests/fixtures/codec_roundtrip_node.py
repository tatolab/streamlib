#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The Python arm of the codec round trip: camera -> encoder -> decoder -> window.

The twin of the camera arm of `codec_roundtrip_rig.rs` under the engine's
`examples/`, authored through the wheel's marker classes instead of `App::add`,
so the vivid drift lock can be measured through a Python-authored graph and
compared against the baseline the Rust rig captured.

All four blocks are native built-ins, so this app declares no Python processor
and the graph spawns no helper process — what runs under the markers is the
engine's own path, unwrapped.

`PIPELINE=python e2e_fixture_psnr_vivid.sh` drives it. The decoder is named
`decoder` because that script derives the channel it exchanges from the live
graph by node name, and it derives it the same way for both arms.
"""

import argparse
import functools

import streamlib
from streamlib import Stream, compile_stream_to_graph, stream

_ENCODER_AND_DECODER_MARKERS_BY_CODEC: dict[str, tuple[type, type]] = {
    "h264": (streamlib.H264Encoder, streamlib.H264Decoder),
    "h265": (streamlib.H265Encoder, streamlib.H265Decoder),
}

# Stated rather than left to the encoder's own default, because the Rust arm
# states it too: one baseline scores both arms only if they present the same
# GOP structure to the decoder.
ENCODER_KEYFRAME_INTERVAL_SECONDS = 2


@functools.cache
def _parse_fixture_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--codec",
        choices=sorted(_ENCODER_AND_DECODER_MARKERS_BY_CODEC),
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
    return parser.parse_args()


@stream
def camera_through_the_codec_into_a_window(stream: Stream) -> None:
    arguments = _parse_fixture_arguments()
    encoder_marker, decoder_marker = _ENCODER_AND_DECODER_MARKERS_BY_CODEC[
        arguments.codec
    ]

    camera = stream.add(
        streamlib.CameraSource,
        name="camera",
        config={"device_id": arguments.camera} if arguments.camera else {},
    )
    encoder = stream.add(
        encoder_marker,
        name="encoder",
        config={"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS},
    )
    decoder = stream.add(decoder_marker, name="decoder")
    display = stream.add(
        streamlib.DisplayWindow,
        name="display",
        config={"title": "streamlib codec round-trip node"},
    )

    stream.connect(camera.output("video"), encoder.input("video"))
    stream.connect(encoder.output("encoded_video"), decoder.input("encoded_video"))
    stream.connect(decoder.output("video"), display.input("video"))


def main() -> None:
    arguments = _parse_fixture_arguments()
    graph = compile_stream_to_graph(camera_through_the_codec_into_a_window)
    runtime = streamlib.Runtime(runtime_name="codec-roundtrip-node")
    runtime.load(graph)

    runtime.host_control_plane()
    runtime.run()


if __name__ == "__main__":
    main()
