# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A StreamLib stream: camera → GPU effect → window, with a CPU meter watching.

`streamlib dev` loads the one `@stream` below by convention — there is no
manifest. Edit `nodes/inverting_effect.py` or `nodes/brightness_meter.py` and
re-run `streamlib dev` to see the change.

Nodes live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from tatolab.stream import CameraSource, DisplayWindow, Stream, stream

from nodes.brightness_meter import BrightnessMeter
from nodes.inverting_effect import InvertingEffect


@stream
def main(stream: Stream) -> None:
    """Camera, inverted, in a window; brightness logged once a second."""
    source = stream.add(CameraSource)
    effect = stream.add(InvertingEffect)
    meter = stream.add(BrightnessMeter)
    window = stream.add(DisplayWindow, config={"title": "StreamLib", "scaling": "fit"})
    stream.connect(source.output("video"), effect.input("video_from_upstream"))
    stream.connect(effect.output("video_to_downstream"), window.input("video"))
    stream.connect(
        effect.output("video_to_downstream"), meter.input("video_from_upstream")
    )
    stream.expose(effect.output("video_to_downstream"))
