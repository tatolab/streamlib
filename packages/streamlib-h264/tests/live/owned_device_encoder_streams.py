# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The test pattern through this wheel's `H264Encoder`, as one stream's graph.

`owned_device_encoders.sh` writes a project whose `stream.py` defines one
`@stream` per encoder, `owned_device_encoder_<n>`, each building this graph,
and loads it on one runtime with `tatolab run --dir`. Each encoder runs in its
own helper on a device that helper owns, and each stream exposes its encoder's
port, so `graph` counts what it writes and the port reads as a track at the
local relay. Node names carry the stream's number, so the runtime's log tells
each helper apart.
"""

from tatolab.stream import StreamBuilder
from tatolab.testpattern import TestPatternSource

from tatolab.h264 import H264Encoder

# Stated rather than left to the encoder's default, so the run's GOP structure
# is the one the engine's codec rig measures over.
ENCODER_KEYFRAME_INTERVAL_SECONDS = 2


def test_pattern_through_an_owned_device_encoder(
    stream_builder: StreamBuilder, stream_number: int
) -> None:
    """Add `test_pattern_<n>` → `encoder_<n>` and expose the encoder's `encoded_video`."""
    test_pattern = stream_builder.add(
        TestPatternSource, name=f"test_pattern_{stream_number}"
    )
    encoder = stream_builder.add(
        H264Encoder,
        name=f"encoder_{stream_number}",
        config={"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS},
    )
    stream_builder.connect(test_pattern.output("video"), encoder.input("video"))
    stream_builder.expose(encoder.output("encoded_video"))
