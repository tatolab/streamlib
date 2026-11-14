# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The codec round trip as a deployment builds it: two streams through one relay.

The sender encodes the test pattern and sends the encoder's port to the
fixture's relay through its own `MoqGateway` → `MoqTrack` → `MoqTrackSend`;
the receiver reads that track there with `MoqTrackReceive` and decodes it into
a sink, which gives the decoder's port a reader so `graph` counts what it
writes. `e2e_fixture_psnr_test_pattern.sh` reads the sender's track with
`tatolab-read-track`, decodes it with ffmpeg and scores the pictures, and reads
the receiver's decoded-frame count off `graph`.

`e2e_fixture_psnr_test_pattern.sh` builds each graph into a project of its own
and loads it with `tatolab open --play --dir`; the generators read their settings from
the environment the graph is built in: `STREAMLIB_FIXTURE_VIDEO_CODEC` picks the
codec (`h264` by default), and `STREAMLIB_FIXTURE_RELAY_URL` names the relay the
fixture started.
"""

import os

from decoded_video_frame_sink import DecodedVideoFrameSink
from tatolab.h264 import H264Decoder, H264Encoder
from tatolab.h265 import H265Decoder, H265Encoder
from tatolab.moq import MoqGateway, MoqTrack, MoqTrackReceive, MoqTrackSend
from tatolab.stream import StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource

_ENCODER_AND_DECODER_MARKERS_BY_CODEC: dict[str, tuple[type, type]] = {
    "h264": (H264Encoder, H264Decoder),
    "h265": (H265Encoder, H265Decoder),
}

# Stated rather than left to the encoder's own default, so the baseline is
# measured over a GOP structure this file names.
ENCODER_KEYFRAME_INTERVAL_SECONDS = 2

#: The broadcast the sender's gateway publishes; the relay serves it as `<this>.hang`.
SENDER_BROADCAST = "codec-roundtrip-sender"

#: The receiver gateway's own broadcast, which carries no track.
RECEIVER_BROADCAST = "codec-roundtrip-receiver"

#: The sender's encoded track in its broadcast.
SENDER_ENCODED_TRACK = "encoded_video"

#: How far the receiver lets newer content run past a missing group before giving it up.
RECEIVER_MAX_AGE_MILLISECONDS = 500


def _codec() -> str:
    codec = os.environ.get("STREAMLIB_FIXTURE_VIDEO_CODEC") or "h264"
    if codec not in _ENCODER_AND_DECODER_MARKERS_BY_CODEC:
        raise ValueError(
            f"STREAMLIB_FIXTURE_VIDEO_CODEC={codec!r} names no codec this round trip "
            f"carries; it carries {', '.join(sorted(_ENCODER_AND_DECODER_MARKERS_BY_CODEC))}"
        )
    return codec


def _fixture_relay_url() -> str:
    relay_url = os.environ.get("STREAMLIB_FIXTURE_RELAY_URL")
    if not relay_url:
        raise ValueError(
            "STREAMLIB_FIXTURE_RELAY_URL is unset, so the round trip names no relay to send through"
        )
    return relay_url


@graph_generator
def test_pattern_through_the_encoder_sent_to_the_relay(stream_builder: StreamBuilder) -> None:
    encoder_marker, _ = _ENCODER_AND_DECODER_MARKERS_BY_CODEC[_codec()]

    test_pattern = stream_builder.add(TestPatternSource, name="test_pattern")
    encoder = stream_builder.add(
        encoder_marker,
        name="encoder",
        config={"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS},
    )
    gateway = stream_builder.add(
        MoqGateway,
        name="gateway",
        config={"kind": "moq-relay", "relay_url": _fixture_relay_url(), "broadcast": SENDER_BROADCAST},
    )
    encoded_track = stream_builder.add(
        MoqTrack, name="encoded_track", config={"gateway": gateway, "track": SENDER_ENCODED_TRACK}
    )
    send = stream_builder.add(MoqTrackSend, name="send", config={"track": encoded_track})

    stream_builder.connect(test_pattern.output("video"), encoder.input("video"))
    stream_builder.connect(encoder.output("encoded_video"), send.input("content"))


@graph_generator
def the_senders_encoded_track_through_the_decoder(stream_builder: StreamBuilder) -> None:
    codec = _codec()
    _, decoder_marker = _ENCODER_AND_DECODER_MARKERS_BY_CODEC[codec]

    gateway = stream_builder.add(
        MoqGateway,
        name="gateway",
        config={"kind": "moq-relay", "relay_url": _fixture_relay_url(), "broadcast": RECEIVER_BROADCAST},
    )
    receive = stream_builder.add(
        MoqTrackReceive,
        name="sender",
        config={
            "gateway": gateway,
            "broadcast": f"{SENDER_BROADCAST}.hang",
            "track": SENDER_ENCODED_TRACK,
            "family": "packets",
            "codec": codec,
            "max_age": RECEIVER_MAX_AGE_MILLISECONDS,
        },
    )
    decoder = stream_builder.add(decoder_marker, name="decoder")
    sink = stream_builder.add(DecodedVideoFrameSink, name="sink")

    stream_builder.connect(receive.output("content"), decoder.input("encoded_video"))
    stream_builder.connect(decoder.output("video"), sink.input("video_from_upstream"))
