# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Streams of the test pattern through this wheel's `H264Encoder`, one each.

`owned_device_encoders.sh` loads up to four of them on one runtime with
`tatolab run`. Each encoder runs in its own helper on a device that helper owns,
and each stream exposes its encoder's port, so `graph` counts what it writes
and the port reads as a track at the local relay. Node names carry the stream's
number, so the runtime's log tells each helper apart.
"""

import tatolab.stream
from tatolab.stream import StreamBuilder, stream

from tatolab.h264 import H264Encoder

# Stated rather than left to the encoder's default, so the run's GOP structure
# is the one the engine's codec rig measures over.
ENCODER_KEYFRAME_INTERVAL_SECONDS = 2


def _test_pattern_through_an_owned_device_encoder(
    stream_builder: StreamBuilder, stream_number: int
) -> None:
    test_pattern = stream_builder.add(
        tatolab.stream.TestPatternSource, name=f"test_pattern_{stream_number}"
    )
    encoder = stream_builder.add(
        H264Encoder,
        name=f"encoder_{stream_number}",
        config={"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS},
    )
    stream_builder.connect(test_pattern.output("video"), encoder.input("video"))
    stream_builder.expose(encoder.output("encoded_video"))


@stream
def owned_device_encoder_1(stream_builder: StreamBuilder) -> None:
    _test_pattern_through_an_owned_device_encoder(stream_builder, 1)


@stream
def owned_device_encoder_2(stream_builder: StreamBuilder) -> None:
    _test_pattern_through_an_owned_device_encoder(stream_builder, 2)


@stream
def owned_device_encoder_3(stream_builder: StreamBuilder) -> None:
    _test_pattern_through_an_owned_device_encoder(stream_builder, 3)


@stream
def owned_device_encoder_4(stream_builder: StreamBuilder) -> None:
    _test_pattern_through_an_owned_device_encoder(stream_builder, 4)
