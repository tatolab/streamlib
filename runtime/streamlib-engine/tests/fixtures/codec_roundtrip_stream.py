# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The codec round trip as a deployment builds it: two streams on one machine.

The sender encodes the test pattern and exposes the encoder's port; the
receiver reads that port's track at the machine's local relay and decodes it.
Nothing reads either from inside the runtime: `e2e_fixture_psnr_test_pattern.sh` reads the
sender's encoded track with `tatolab-read-track`, decodes it with ffmpeg and
scores the pictures, and reads the receiver's decoded-frame count off `graph`,
which counts a port's writes once something reads it — the receiver exposes
its decoded port for that.

`e2e_fixture_psnr_test_pattern.sh` loads each with `tatolab run` into a
`tatolabd` it starts with the streams' settings in its environment, which the
compile inherits: `STREAMLIB_FIXTURE_VIDEO_CODEC` picks the codec (`h264` by
default). The sender is loaded as `codec_roundtrip_sender`, the name the
receiver's broadcast is built from.
"""

import json
import os
import urllib.request

import tatolab.stream
from tatolab.stream import StreamBuilder, stream

_ENCODER_AND_DECODER_MARKERS_BY_CODEC: dict[str, tuple[type, type]] = {
    "h264": (tatolab.stream.H264Encoder, tatolab.stream.H264Decoder),
    "h265": (tatolab.stream.H265Encoder, tatolab.stream.H265Decoder),
}

# Stated rather than left to the encoder's own default, so the baseline is
# measured over a GOP structure this file names.
ENCODER_KEYFRAME_INTERVAL_SECONDS = 2

#: The name the harness loads the sender under.
SENDER_STREAM_NAME = "codec_roundtrip_sender"

#: The sender's exposed port, as a track of its broadcast.
SENDER_ENCODED_TRACK = "encoder/encoded_video"

#: The machine's loopback HTTP, whose listing names the machine.
LOOPBACK_HTTP_LISTING_URL = "http://127.0.0.1:4446/"


def _codec_markers() -> tuple[type, type]:
    codec = os.environ.get("STREAMLIB_FIXTURE_VIDEO_CODEC") or "h264"
    if codec not in _ENCODER_AND_DECODER_MARKERS_BY_CODEC:
        raise ValueError(
            f"STREAMLIB_FIXTURE_VIDEO_CODEC={codec!r} names no codec this round trip "
            f"carries; it carries {', '.join(sorted(_ENCODER_AND_DECODER_MARKERS_BY_CODEC))}"
        )
    return _ENCODER_AND_DECODER_MARKERS_BY_CODEC[codec]


def _this_machines_name() -> str:
    """This machine's name, read off the runtime's loopback listing."""
    with urllib.request.urlopen(LOOPBACK_HTTP_LISTING_URL, timeout=5) as listing:
        return json.load(listing)["machine"]


@stream
def test_pattern_through_the_encoder_exposing_its_encoded_port(
    stream_builder: StreamBuilder,
) -> None:
    encoder_marker, _ = _codec_markers()

    test_pattern = stream_builder.add(
        tatolab.stream.TestPatternSource, name="test_pattern"
    )
    encoder = stream_builder.add(
        encoder_marker,
        name="encoder",
        config={"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS},
    )

    stream_builder.connect(test_pattern.output("video"), encoder.input("video"))
    stream_builder.expose(encoder.output("encoded_video"))


@stream
def the_senders_encoded_track_through_the_decoder(stream_builder: StreamBuilder) -> None:
    _, decoder_marker = _codec_markers()

    relay_track = stream_builder.add(
        tatolab.stream.RelayTrackSource,
        name="sender",
        config={
            "broadcast": f"{_this_machines_name()}/{SENDER_STREAM_NAME}.hang",
            "track": SENDER_ENCODED_TRACK,
        },
    )
    decoder = stream_builder.add(decoder_marker, name="decoder")

    stream_builder.connect(relay_track.output("track"), decoder.input("encoded_video"))
    # Exposed so the decoded port has a reader, and `graph` counts what it writes.
    stream_builder.expose(decoder.output("video"))
