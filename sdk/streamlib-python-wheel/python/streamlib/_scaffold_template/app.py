"""A StreamLib app: camera → effect → window.

`streamlib dev` finds `setup(rt)` below by convention — there is no manifest and
no `main()`. Edit `processors/inverting_effect.py` and re-run `streamlib dev` to
see the change.

Processors live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from streamlib import CameraSource, DisplayWindow, Runtime

from processors.inverting_effect import InvertingEffect


def setup(rt: Runtime) -> None:
    source = rt.add(CameraSource)
    effect = rt.add(InvertingEffect)
    window = rt.add(DisplayWindow, config={"title": "StreamLib", "scaling": "fit"})

    rt.connect(source.output("video"), effect.input("video_from_upstream"))
    rt.connect(effect.output("video_to_downstream"), window.input("video"))
