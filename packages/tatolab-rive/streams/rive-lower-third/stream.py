# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Rive lower third composited over a camera, its text driven by data, seen in a viewer.

Put a `.riv` file named `lower_third.riv` beside this file, or point `RIV_PATH`
at one, whose artboard's default view-model instance has a string property at
`TITLE_PROPERTY_PATH`. Once a second the ticker sends a new title on the
source's `data` input, and `AlphaOverCompositor` lays the animation, transparent
around the card, over the camera. Watch the compositor's `video` in Tato
Studio's viewer.
"""

from pathlib import Path

from tatolab.stream import StreamBuilder, stream

from lower_third_title_ticker import LowerThirdTitleTicker
from tatolab.camera import CameraSource
from tatolab.compositor import AlphaOverCompositor
from tatolab.rive import RiveSource

# A V4L2 node such as `/dev/video0`; `None` opens the first capture device found.
CAMERA_DEVICE_PATH: str | None = None

CANVAS_WIDTH = 1920
CANVAS_HEIGHT = 1080
# The artboard is fit inside the source's frame, so a frame of the sample
# artboard's aspect (500x114) holds the card edge to edge.
LOWER_THIRD_WIDTH = 1000
LOWER_THIRD_HEIGHT = 228
LOWER_THIRD_MARGIN = 80

# Absolute, because the source's process does not run in this folder.
RIV_PATH = str(Path(__file__).resolve().parent / "lower_third.riv")
TITLE_PROPERTY_PATH = "Name"
# The sample lower third's state machine reaches its entrance ("In") from its
# first state only when both triggers fire in the same message.
ENTRANCE_TRIGGER_PATHS = ["IN", "OUT"]


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The camera on `layer_0` and the lower third over it at 1080p60."""
    camera_config: dict[str, object] = {"max_width": CANVAS_WIDTH, "max_height": CANVAS_HEIGHT}
    if CAMERA_DEVICE_PATH is not None:
        camera_config["device_path"] = CAMERA_DEVICE_PATH
    camera = stream_builder.add(CameraSource, config=camera_config)
    rive = stream_builder.add(
        RiveSource,
        config={
            "riv_path": RIV_PATH,
            "width": LOWER_THIRD_WIDTH,
            "height": LOWER_THIRD_HEIGHT,
            "frame_rate": 60,
        },
    )
    ticker = stream_builder.add(
        LowerThirdTitleTicker,
        config={
            "property_path": TITLE_PROPERTY_PATH,
            "entrance_trigger_paths": ENTRANCE_TRIGGER_PATHS,
        },
    )
    compositor = stream_builder.add(
        AlphaOverCompositor,
        config={
            "width": CANVAS_WIDTH,
            "height": CANVAS_HEIGHT,
            "layers": [
                {},
                {
                    "x": LOWER_THIRD_MARGIN,
                    "y": CANVAS_HEIGHT - LOWER_THIRD_HEIGHT - LOWER_THIRD_MARGIN,
                    "opacity": 1.0,
                },
            ],
        },
    )
    stream_builder.connect(ticker.output("data"), rive.input("data"))
    stream_builder.connect(camera.output("video"), compositor.input("layer_0"))
    stream_builder.connect(rive.output("video"), compositor.input("layer_1"))
