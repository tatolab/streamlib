# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: test pattern → GPU effect, with a CPU meter watching.

`tatolab dev` loads the one `@stream` below by convention — there is no
manifest — and restarts it when you edit `nodes/inverting_effect.py` or
`nodes/brightness_meter.py`.

Nodes live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from tatolab.stream import StreamBuilder, TestPatternSource, stream

from nodes.brightness_meter import BrightnessMeter
from nodes.inverting_effect import InvertingEffect


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The test pattern, inverted and exposed; brightness logged once a second."""
    source = stream_builder.add(TestPatternSource)
    effect = stream_builder.add(InvertingEffect)
    meter = stream_builder.add(BrightnessMeter)
    stream_builder.connect(source.output("video"), effect.input("video_from_upstream"))
    stream_builder.connect(
        effect.output("video_to_downstream"), meter.input("video_from_upstream")
    )
    stream_builder.expose(effect.output("video_to_downstream"))
