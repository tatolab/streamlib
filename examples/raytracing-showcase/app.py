# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A StreamLib app: the same scene rendered twice, cut down the middle.

Left half rasterized, right half ray traced, so what ray tracing buys is the
difference between them and nothing else. Two renderers fan into a compositor
and the composite goes to a window; each of the three is an ordinary Python
class in its own child interpreter, and each drives a different kind of engine
kernel — graphics, ray tracing, compute.

`streamlib run` finds `setup(rt)` below by convention — there is no manifest
and no `main()`. `DisplayWindow` is a native built-in that ships inside the
wheel. Nothing here needs a camera or any other device: the picture is
synthetic all the way down.

Processors live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from processors.rasterized_scene_renderer import RasterizedSceneRenderer
from processors.ray_traced_scene_renderer import RayTracedSceneRenderer
from processors.split_screen_compositor import SplitScreenCompositor

from streamlib import DisplayWindow, Runtime

FRAME_WIDTH = 1280
FRAME_HEIGHT = 720


def setup(rt: Runtime) -> None:
    frame_size: dict[str, object] = {"width": FRAME_WIDTH, "height": FRAME_HEIGHT}

    # Both renderers draw the whole frame, and the compositor takes half of
    # each: the two sides then show the same view of the same scene at the
    # same instant, which is what makes the cut a comparison rather than two
    # pictures side by side.
    rasterizer = rt.add(RasterizedSceneRenderer, config=frame_size)
    ray_tracer = rt.add(RayTracedSceneRenderer, config=frame_size)
    # These three are ordinary constructor keywords with ordinary Python
    # defaults — `config` is how a processor's own `__init__` is called, and
    # nothing about them is streamlib surface. The labels reach the compositor
    # before it builds its kernel, so they end up baked into its GLSL.
    compositor = rt.add(
        SplitScreenCompositor,
        config={
            "split_fraction": 0.5,
            "left_label": "RTX OFF",
            "right_label": "RTX ON",
        },
    )
    window = rt.add(
        DisplayWindow,
        config={
            "title": "StreamLib Ray Tracing — rasterized left, traced right",
            "width": FRAME_WIDTH,
            "height": FRAME_HEIGHT,
            "scaling": "fit",
        },
    )

    rt.connect(
        rasterizer.output("rasterized_frame_to_downstream"),
        compositor.input("rasterized_frame_from_upstream"),
    )
    rt.connect(
        ray_tracer.output("ray_traced_frame_to_downstream"),
        compositor.input("ray_traced_frame_from_upstream"),
    )
    rt.connect(
        compositor.output("split_screen_frame_to_downstream"), window.input("video")
    )
