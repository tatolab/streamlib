# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Streams wiring the audio and video nodes into each other with no adapter
between them, each run from the suite project to prove the engine loads the
wiring as the nodes publish their ports."""

from tatolab.h264 import H264Decoder, H264Encoder
from tatolab.h265 import H265Decoder, H265Encoder
from tatolab.microphone import MicrophoneSource
from tatolab.opus import OpusDecoder, OpusEncoder
from tatolab.speaker import SpeakerSink
from tatolab.testpattern import TestPatternSource
from tatolab.stream import (
    StreamBuilder,
    graph_generator,
)


@graph_generator
def microphone_wired_straight_into_a_speaker(stream_builder: StreamBuilder) -> None:
    microphone = stream_builder.add(MicrophoneSource)
    speaker = stream_builder.add(SpeakerSink)
    stream_builder.connect(microphone.output("audio"), speaker.input("audio"))


@graph_generator
def microphone_through_the_opus_round_trip_into_a_speaker(stream_builder: StreamBuilder) -> None:
    microphone = stream_builder.add(MicrophoneSource)
    encoder = stream_builder.add(OpusEncoder)
    decoder = stream_builder.add(OpusDecoder)
    speaker = stream_builder.add(SpeakerSink)
    stream_builder.connect(microphone.output("audio"), encoder.input("audio"))
    stream_builder.connect(encoder.output("encoded_audio"), decoder.input("encoded_audio"))
    stream_builder.connect(decoder.output("audio"), speaker.input("audio"))


def _add_a_codec_round_trip(
    stream_builder: StreamBuilder,
    encoder_class: "type[H264Encoder] | type[H265Encoder]",
    decoder_class: "type[H264Decoder] | type[H265Decoder]",
) -> None:
    pattern = stream_builder.add(TestPatternSource)
    encoder = stream_builder.add(encoder_class)
    decoder = stream_builder.add(decoder_class)
    stream_builder.connect(pattern.output("video"), encoder.input("video"))
    stream_builder.connect(encoder.output("encoded_video"), decoder.input("encoded_video"))


@graph_generator
def h264_round_trip(stream_builder: StreamBuilder) -> None:
    _add_a_codec_round_trip(stream_builder, H264Encoder, H264Decoder)


@graph_generator
def h265_round_trip(stream_builder: StreamBuilder) -> None:
    _add_a_codec_round_trip(stream_builder, H265Encoder, H265Decoder)
