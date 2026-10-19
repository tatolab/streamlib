# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: the sample animation → GPU effect → H.264, shared publicly.

`tatolab dev` loads the one `@stream` below by convention — there is no
manifest — and restarts it when you edit `nodes/inverting_effect.py` or
`nodes/brightness_meter.py`. While it runs, http://127.0.0.1:4446/ lists the
stream's public track and http://127.0.0.1:4446/watch/<machine>/main plays it
in a browser.

Nodes live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from tatolab.stream import (
    Exposure,
    H264Encoder,
    StreamBuilder,
    TestPatternSource,
    stream,
)

from nodes.brightness_meter import BrightnessMeter
from nodes.inverting_effect import InvertingEffect


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The sample animation, inverted, encoded and exposed; brightness logged once a second."""
    # Point it at your camera: `stream_builder.add(CameraSource)`, imported
    # from `tatolab.stream`, publishes on the same `video` port.
    source = stream_builder.add(TestPatternSource)
    effect = stream_builder.add(InvertingEffect)
    meter = stream_builder.add(BrightnessMeter)
    encoder = stream_builder.add(H264Encoder)
    stream_builder.connect(source.output("video"), effect.input("video_from_upstream"))
    stream_builder.connect(
        effect.output("video_to_downstream"), meter.input("video_from_upstream")
    )
    stream_builder.connect(effect.output("video_to_downstream"), encoder.input("video"))
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PUBLIC)
