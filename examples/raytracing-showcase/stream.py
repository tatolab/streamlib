# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: one scene rasterized and ray traced, cut down the middle, shared on this machine.

`tatolab dev` loads the one `@stream` below by convention — there is no
manifest. Two renderers fan into a compositor; each of the three is a node of
this project, in its own child interpreter, driving a different kernel kind —
graphics, ray tracing, compute. The cut goes through an `H264Encoder`, exposed,
so a browser plays it. Nothing here needs a camera: the picture is synthetic all
the way down.

Nodes live in their own modules, never in this file: each one runs in its own
child interpreter, which imports the class by name.
"""

from tatolab.stream import (
    Exposure,
    H264Encoder,
    StreamBuilder,
    stream,
)

from nodes.rasterized_scene_renderer import RasterizedSceneRenderer
from nodes.ray_traced_scene_renderer import RayTracedSceneRenderer
from nodes.split_screen_compositor import SplitScreenCompositor

# The extent both halves render at, and so the extent of the cut.
FRAME_WIDTH = 1280
FRAME_HEIGHT = 720

# Where the cut falls: 0.0 is all ray traced, 1.0 all rasterized.
SPLIT_FRACTION = 0.5

# The text over each half. Generated into the compositor's GLSL at `setup()`;
# the font covers A-Z, 0-9, space, hyphen and full stop.
LEFT_LABEL = "RTX OFF"
RIGHT_LABEL = "RTX ON"


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The scene rasterized left and ray traced right; the cut encoded and exposed."""
    frame_extent = {"width": FRAME_WIDTH, "height": FRAME_HEIGHT}
    # Both renderers draw the whole frame and the compositor takes half of
    # each, so the two sides show one view of one scene. They agree because
    # each takes its orbit phase from the clock every node shares.
    rasterizer = stream_builder.add(RasterizedSceneRenderer, config=frame_extent)
    ray_tracer = stream_builder.add(RayTracedSceneRenderer, config=frame_extent)
    compositor = stream_builder.add(
        SplitScreenCompositor,
        config={
            "split_fraction": SPLIT_FRACTION,
            "left_label": LEFT_LABEL,
            "right_label": RIGHT_LABEL,
        },
    )
    split_screen_encoder = stream_builder.add(H264Encoder, name="split-screen-encoder")

    stream_builder.connect(
        rasterizer.output("rasterized_frame_to_downstream"),
        compositor.input("rasterized_frame_from_upstream"),
    )
    stream_builder.connect(
        ray_tracer.output("ray_traced_frame_to_downstream"),
        compositor.input("ray_traced_frame_from_upstream"),
    )
    stream_builder.connect(
        compositor.output("split_screen_frame_to_downstream"),
        split_screen_encoder.input("video"),
    )

    stream_builder.expose(
        split_screen_encoder.output("encoded_video"), Exposure.PRIVATE
    )
