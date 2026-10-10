# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Streams wiring the audio and video built-ins into each other with no adapter
between them, each run from the suite project to prove the engine loads the
wiring as the built-ins publish their ports."""

import tatolab.stream
from tatolab.stream import (
    H264Decoder,
    H264Encoder,
    H265Decoder,
    H265Encoder,
    Mp4Sink,
    OpusDecoder,
    OpusEncoder,
    StreamBuilder,
    stream,
)

#: The sink opens its file at `setup()`, which a graph loaded and never run does
#: not reach, so nothing is ever written here.
NEVER_OPENED_RECORDING_PATH = "/nonexistent-streamlib-test/never-opened.mp4"


@stream
def microphone_wired_straight_into_a_speaker(stream_builder: StreamBuilder) -> None:
    microphone = stream_builder.add(tatolab.stream.MicrophoneSource)
    speaker = stream_builder.add(tatolab.stream.SpeakerSink)
    stream_builder.connect(microphone.output("audio"), speaker.input("audio"))


@stream
def microphone_through_the_opus_round_trip_into_a_speaker(stream_builder: StreamBuilder) -> None:
    microphone = stream_builder.add(tatolab.stream.MicrophoneSource)
    encoder = stream_builder.add(OpusEncoder)
    decoder = stream_builder.add(OpusDecoder)
    speaker = stream_builder.add(tatolab.stream.SpeakerSink)
    stream_builder.connect(microphone.output("audio"), encoder.input("audio"))
    stream_builder.connect(encoder.output("encoded_audio"), decoder.input("encoded_audio"))
    stream_builder.connect(decoder.output("audio"), speaker.input("audio"))


@stream
def two_microphone_encoder_pairs_into_one_mp4_sink(stream_builder: StreamBuilder) -> None:
    sink = stream_builder.add(Mp4Sink, config={"path": NEVER_OPENED_RECORDING_PATH})
    for _ in range(2):
        microphone = stream_builder.add(tatolab.stream.MicrophoneSource)
        encoder = stream_builder.add(tatolab.stream.OpusEncoder)
        stream_builder.connect(microphone.output("audio"), encoder.input("audio"))
        stream_builder.connect(encoder.output("encoded_audio"), sink.input("tracks"))


def _add_a_codec_round_trip_into_a_window(
    stream_builder: StreamBuilder,
    encoder_class: "type[H264Encoder] | type[H265Encoder]",
    decoder_class: "type[H264Decoder] | type[H265Decoder]",
) -> None:
    pattern = stream_builder.add(tatolab.stream.TestPatternSource)
    encoder = stream_builder.add(encoder_class)
    decoder = stream_builder.add(decoder_class)
    window = stream_builder.add(tatolab.stream.DisplayWindow)
    stream_builder.connect(pattern.output("video"), encoder.input("video"))
    stream_builder.connect(encoder.output("encoded_video"), decoder.input("encoded_video"))
    stream_builder.connect(decoder.output("video"), window.input("video"))


@stream
def h264_round_trip_into_a_window(stream_builder: StreamBuilder) -> None:
    _add_a_codec_round_trip_into_a_window(stream_builder, H264Encoder, H264Decoder)


@stream
def h265_round_trip_into_a_window(stream_builder: StreamBuilder) -> None:
    _add_a_codec_round_trip_into_a_window(stream_builder, H265Encoder, H265Decoder)
