# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The hosted-relay proof's sender: the test pattern encoded twice.

`encoder`'s port is public, so the publisher puts it at every relay the
machine has joined, the hosted ones among them; `private_encoder`'s is
private, so it is at the local relay's loopback listener alone.
`hosted_relays_roundtrip.sh` loads it with `tatolab run --name
hosted_relays_sender` and reads both tracks back off each hosted relay.
"""

import tatolab.stream
from tatolab.stream import Exposure, StreamBuilder, stream

# The colour rig's GOP, so its h264 baseline scores what is read back here.
ENCODER_KEYFRAME_INTERVAL_SECONDS = 2


@stream
def the_test_pattern_encoded_with_one_port_public_and_one_private(
    stream_builder: StreamBuilder,
) -> None:
    test_pattern = stream_builder.add(
        tatolab.stream.TestPatternSource, name="test_pattern"
    )
    encoder_config = {"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS}
    public_encoder = stream_builder.add(
        tatolab.stream.H264Encoder, name="encoder", config=encoder_config
    )
    private_encoder = stream_builder.add(
        tatolab.stream.H264Encoder, name="private_encoder", config=encoder_config
    )

    stream_builder.connect(test_pattern.output("video"), public_encoder.input("video"))
    stream_builder.connect(test_pattern.output("video"), private_encoder.input("video"))
    stream_builder.expose(public_encoder.output("encoded_video"), Exposure.PUBLIC)
    stream_builder.expose(private_encoder.output("encoded_video"), Exposure.PRIVATE)
