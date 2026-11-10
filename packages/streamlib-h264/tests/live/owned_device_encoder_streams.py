# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The test pattern through this wheel's `H264Encoder`, as one stream's graph.

`owned_device_encoders.sh` writes a project whose `stream.py` defines one
`@graph_generator` per encoder, `owned_device_encoder_<n>`, each building this graph,
and loads it on one runtime with `tatolab start --dir`. Each encoder runs in its
own helper on a device that helper owns, and each stream sends its encoder's
port to the fixture's relay through `MoqGateway` → `MoqTrack` → `MoqTrackSend`,
so `graph` counts what it writes and `owned_device_encoders.sh` reads it there
as a track. Node names carry the stream's number, so the runtime's log tells
each helper apart.

`STREAMLIB_FIXTURE_RELAY_URL`, in the environment of the runtime
`owned_device_encoders.sh` starts, which the compile inherits, names the relay
the fixture started.
"""

import os

from tatolab.moq import MoqGateway, MoqTrack, MoqTrackSend
from tatolab.stream import StreamBuilder
from tatolab.testpattern import TestPatternSource

from tatolab.h264 import H264Encoder

# Stated rather than left to the encoder's default, so the run's GOP structure
# is the one the engine's codec rig measures over.
ENCODER_KEYFRAME_INTERVAL_SECONDS = 2

FIXTURE_RELAY_URL_VARIABLE = "STREAMLIB_FIXTURE_RELAY_URL"

#: The encoder's track in each stream's broadcast.
ENCODED_VIDEO_TRACK = "encoded_video"


def broadcast_of_owned_device_encoder_stream(stream_number: int) -> str:
    """The broadcast stream `<n>`'s gateway publishes; the relay serves it as `<this>.hang`."""
    return f"owned-device-encoder-{stream_number}"


def test_pattern_through_an_owned_device_encoder(
    stream_builder: StreamBuilder, stream_number: int
) -> None:
    """Add `test_pattern_<n>` → `encoder_<n>` and send the encoder's `encoded_video` to the relay."""
    relay_url = os.environ.get(FIXTURE_RELAY_URL_VARIABLE)
    if not relay_url:
        raise SystemExit(f"{FIXTURE_RELAY_URL_VARIABLE} is unset, so the stream names no relay")
    test_pattern = stream_builder.add(
        TestPatternSource, name=f"test_pattern_{stream_number}"
    )
    encoder = stream_builder.add(
        H264Encoder,
        name=f"encoder_{stream_number}",
        config={"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS},
    )
    gateway = stream_builder.add(
        MoqGateway,
        name=f"gateway_{stream_number}",
        config={
            "kind": "moq-relay",
            "relay_url": relay_url,
            "broadcast": broadcast_of_owned_device_encoder_stream(stream_number),
        },
    )
    encoded_video_track = stream_builder.add(
        MoqTrack,
        name=f"encoded_video_track_{stream_number}",
        config={"gateway": gateway, "track": ENCODED_VIDEO_TRACK},
    )
    send = stream_builder.add(
        MoqTrackSend, name=f"send_{stream_number}", config={"track": encoded_video_track}
    )
    stream_builder.connect(test_pattern.output("video"), encoder.input("video"))
    stream_builder.connect(encoder.output("encoded_video"), send.input("content"))
