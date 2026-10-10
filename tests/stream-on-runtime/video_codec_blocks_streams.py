# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_video_codec_blocks.py` runs from the suite project."""

import tatolab.stream
from tatolab.stream import H264Decoder, H264Encoder, H265Decoder, H265Encoder, StreamBuilder, stream

from video_codec_blocks_probes import (
    DecodedVideoFrameProbe,
    EncodedFrameProbe,
    EncodedFrameTimestampProbe,
)


# One second at the pattern's 30 fps. The encoder resolves its rate from the
# frame, and the pattern's bag carries 30, so this is 30 frames per group.
KEYFRAME_INTERVAL_SECONDS = 1


@stream
def an_h264_encoder_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(H264Encoder)


@stream
def an_h264_decoder_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(H264Decoder)


@stream
def an_h265_encoder_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(H265Encoder)


@stream
def an_h265_decoder_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(H265Decoder)


def _add_a_codec_round_trip_with_probes(
    stream_builder: StreamBuilder,
    encoder_class: "type[H264Encoder] | type[H265Encoder]",
    decoder_class: "type[H264Decoder] | type[H265Decoder]",
) -> None:
    """A test pattern encoded and decoded back, with no Python in the frame path.

    `TestPatternSource → <codec>Encoder → <codec>Decoder → DecodedVideoFrameProbe`.
    The pattern's extent is 320×180, an extent both codecs pad (to 320×192:
    the 16-sample macroblock and the 64-sample CTU agree on it), so the decoded
    probe seeing 320×180 is the conformance crop proven from Python.

    Two more probes fan off the encoded link beside the decoder, which is what
    makes one run prove both halves: what the encoder published, and that the
    decoder carried each frame's own stamp through. The encoder asks for a
    1-second keyframe interval — at the pattern's 30 fps that is a sync point
    every 30 frames — so the probes' window spans a group boundary rather than
    asserting about a single group.
    """
    pattern = stream_builder.add(
        tatolab.stream.TestPatternSource, config={"width": 320, "height": 180}
    )
    encoder = stream_builder.add(
        encoder_class,
        config={"keyframe_interval_seconds": KEYFRAME_INTERVAL_SECONDS},
    )
    decoder = stream_builder.add(decoder_class)
    encoded_frame_probe = stream_builder.add(EncodedFrameProbe)
    encoded_frame_timestamp_probe = stream_builder.add(EncodedFrameTimestampProbe)
    decoded_frame_probe = stream_builder.add(DecodedVideoFrameProbe)
    stream_builder.connect(pattern.output("video"), encoder.input("video"))
    stream_builder.connect(encoder.output("encoded_video"), decoder.input("encoded_video"))
    stream_builder.connect(
        encoder.output("encoded_video"),
        encoded_frame_probe.input("encoded_video_from_upstream"),
    )
    stream_builder.connect(
        encoder.output("encoded_video"),
        encoded_frame_timestamp_probe.input("encoded_video_from_upstream"),
    )
    stream_builder.connect(
        decoder.output("video"), decoded_frame_probe.input("video_from_upstream")
    )


@stream
def an_h264_round_trip_with_probes(stream_builder: StreamBuilder) -> None:
    _add_a_codec_round_trip_with_probes(stream_builder, H264Encoder, H264Decoder)


@stream
def an_h265_round_trip_with_probes(stream_builder: StreamBuilder) -> None:
    _add_a_codec_round_trip_with_probes(stream_builder, H265Encoder, H265Decoder)
