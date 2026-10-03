# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A StreamLib app: camera → GPU effect → window, with a CPU meter watching.

`streamlib dev` finds `setup(rt)` below by convention — there is no manifest and
no `main()`. Edit `nodes/inverting_effect.py` or
`nodes/brightness_meter.py` and re-run `streamlib dev` to see the change.

Nodes live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from streamlib import CameraSource, DisplayWindow, Runtime

from nodes.brightness_meter import BrightnessMeter
from nodes.inverting_effect import InvertingEffect


def setup(rt: Runtime) -> None:
    source = rt.add(CameraSource)
    effect = rt.add(InvertingEffect)
    meter = rt.add(BrightnessMeter)
    window = rt.add(DisplayWindow, config={"title": "StreamLib", "scaling": "fit"})

    rt.connect(source.output("video"), effect.input("video_from_upstream"))
    # One output, two readers: the window shows the frame, the meter measures it.
    rt.connect(effect.output("video_to_downstream"), window.input("video"))
    rt.connect(effect.output("video_to_downstream"), meter.input("video_from_upstream"))
