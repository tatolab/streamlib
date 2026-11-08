# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: the sample animation through a GPU effect, its brightness metered.

`tatolab dev` loads every `@stream` below by convention — there is no
manifest — each as its own stream, and loads them again when you edit
`nodes/halftone_lens_effect.py` or `nodes/brightness_meter.py`. While it runs,
open the effect's output in Tato Studio's viewer to watch it.

Nodes live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from tatolab.stream import StreamBuilder, stream
from tatolab.testpattern import TestPatternSource

from nodes.brightness_meter import BrightnessMeter
from nodes.halftone_lens_effect import HalftoneLensEffect


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The sample animation through the lens; its brightness logged each second."""
    # Point it at your camera: `stream_builder.add(CameraSource)`, imported
    # from `tatolab.camera` (`pip install tatolab-camera`), publishes on the
    # same `video` port.
    source = stream_builder.add(TestPatternSource)
    effect = stream_builder.add(HalftoneLensEffect)
    meter = stream_builder.add(BrightnessMeter)
    stream_builder.connect(source.output("video"), effect.input("video_from_upstream"))
    stream_builder.connect(
        effect.output("video_to_downstream"), meter.input("video_from_upstream")
    )
