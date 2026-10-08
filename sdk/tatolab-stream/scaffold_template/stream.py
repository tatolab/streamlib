# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A StreamLib stream: camera → GPU effect → window, with a CPU meter watching.

`tatolab dev` loads the one `@stream` below by convention — there is no
manifest — and restarts it when you edit `nodes/inverting_effect.py` or
`nodes/brightness_meter.py`.

Nodes live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from tatolab.stream import CameraSource, DisplayWindow, StreamBuilder, stream

from nodes.brightness_meter import BrightnessMeter
from nodes.inverting_effect import InvertingEffect


@stream
def main(stream_builder: StreamBuilder) -> None:
    """Camera, inverted, in a window; brightness logged once a second."""
    source = stream_builder.add(CameraSource)
    effect = stream_builder.add(InvertingEffect)
    meter = stream_builder.add(BrightnessMeter)
    window = stream_builder.add(
        DisplayWindow, config={"title": "StreamLib", "scaling": "fit"}
    )
    stream_builder.connect(source.output("video"), effect.input("video_from_upstream"))
    stream_builder.connect(effect.output("video_to_downstream"), window.input("video"))
    stream_builder.connect(
        effect.output("video_to_downstream"), meter.input("video_from_upstream")
    )
    stream_builder.expose(effect.output("video_to_downstream"))
