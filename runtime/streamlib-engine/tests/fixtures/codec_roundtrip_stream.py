# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The Python arm of the codec round trip: camera -> encoder -> decoder -> window.

The twin of the camera arm of `codec_roundtrip_rig.rs` under the engine's
`examples/`, authored through `tatolab.stream`'s built-in classes instead of
`App::add`, so the vivid drift lock can be measured through a Python-authored
graph and compared against the baseline the Rust rig captured.

All four blocks are native built-ins, so this stream declares no Python node
and the graph spawns no helper process — what runs under the markers is the
engine's own path, unwrapped.

`PIPELINE=python e2e_fixture_psnr_vivid.sh` loads it with `tatolab run` into a
`tatolabd` it starts with the stream's settings in its environment, which the
compile inherits: `STREAMLIB_FIXTURE_VIDEO_CODEC` picks the codec (`h264` by
default) and `STREAMLIB_CAMERA_DEVICE` the V4L2 node, else the first the engine
finds. The decoder is named `decoder` because that script derives the
channel it exchanges from the live graph by node name, and it derives it the
same way for both arms.
"""

import os

import tatolab.stream
from tatolab.stream import StreamBuilder, stream

_ENCODER_AND_DECODER_MARKERS_BY_CODEC: dict[str, tuple[type, type]] = {
    "h264": (tatolab.stream.H264Encoder, tatolab.stream.H264Decoder),
    "h265": (tatolab.stream.H265Encoder, tatolab.stream.H265Decoder),
}

# Stated rather than left to the encoder's own default, because the Rust arm
# states it too: one baseline scores both arms only if they present the same
# GOP structure to the decoder.
ENCODER_KEYFRAME_INTERVAL_SECONDS = 2


@stream
def camera_through_the_codec_into_a_window(stream_builder: StreamBuilder) -> None:
    codec = os.environ.get("STREAMLIB_FIXTURE_VIDEO_CODEC") or "h264"
    if codec not in _ENCODER_AND_DECODER_MARKERS_BY_CODEC:
        raise ValueError(
            f"STREAMLIB_FIXTURE_VIDEO_CODEC={codec!r} names no codec this round trip "
            f"carries; it carries {', '.join(sorted(_ENCODER_AND_DECODER_MARKERS_BY_CODEC))}"
        )
    encoder_marker, decoder_marker = _ENCODER_AND_DECODER_MARKERS_BY_CODEC[codec]
    camera_device = os.environ.get("STREAMLIB_CAMERA_DEVICE")

    camera = stream_builder.add(
        tatolab.stream.CameraSource,
        name="camera",
        config={"device_id": camera_device} if camera_device else {},
    )
    encoder = stream_builder.add(
        encoder_marker,
        name="encoder",
        config={"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS},
    )
    decoder = stream_builder.add(decoder_marker, name="decoder")
    display = stream_builder.add(
        tatolab.stream.DisplayWindow,
        name="display",
        config={"title": "streamlib codec round-trip node"},
    )

    stream_builder.connect(camera.output("video"), encoder.input("video"))
    stream_builder.connect(encoder.output("encoded_video"), decoder.input("encoded_video"))
    stream_builder.connect(decoder.output("video"), display.input("video"))
