# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: a live lower third and a Rive station bug over the camera, at 1080p60.

    CameraSource ───────────────────────────────────────────▶ layer_0 ─┐
    LowerThirdRundown ─ data ─▶ BrowserSource (lower_third.html) ─▶ layer_1 ─┼─ AlphaOverCompositor ─ H264Encoder
    RiveSource (station_bug.riv) ────────────────────────────▶ layer_2 ─┘

Every few seconds `LowerThirdRundown` sends the page a new `LowerThirdTemplate`,
and the page swaps its text and style without the stream restarting. Watch it at
http://127.0.0.1:4446/watch/<machine>/main.
"""

from pathlib import Path

from tatolab.browser import BrowserSource
from tatolab.camera import CameraSource
from tatolab.compositor import AlphaOverCompositor
from tatolab.h264 import H264Encoder
from tatolab.rive import RiveSource
from tatolab.stream import Exposure, StreamBuilder, stream

from lower_third.nodes.lower_third_rundown import LowerThirdRundown

# The capture device: a V4L2 path (`/dev/video2`) on Linux, an AVFoundation
# unique ID on macOS. `None` opens the first capture device found.
CAMERA_DEVICE_ID: str | None = None

# Absolute, because each node's process does not run in this folder.
TEMPLATES_DIRECTORY = Path(__file__).resolve().parent / "templates"
LOWER_THIRD_PAGE_URL = (TEMPLATES_DIRECTORY / "lower_third.html").as_uri()
STATION_BUG_RIV_PATH = str(TEMPLATES_DIRECTORY / "station_bug.riv")

CANVAS_WIDTH = 1920
CANVAS_HEIGHT = 1080
FRAME_RATE = 60
# The page's own size, set in `templates/lower_third.html`.
LOWER_THIRD_WIDTH = 1280
LOWER_THIRD_HEIGHT = 220
STATION_BUG_SIZE = 200
SAFE_AREA_MARGIN = 64


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The camera, the lower third and the station bug, composited, encoded and exposed."""
    camera_config: dict[str, object] = {"max_width": CANVAS_WIDTH, "max_height": CANVAS_HEIGHT}
    if CAMERA_DEVICE_ID is not None:
        camera_config["device_id"] = CAMERA_DEVICE_ID
    camera = stream_builder.add(CameraSource, config=camera_config)
    lower_third_page = stream_builder.add(
        BrowserSource,
        config={
            "url": LOWER_THIRD_PAGE_URL,
            "width": LOWER_THIRD_WIDTH,
            "height": LOWER_THIRD_HEIGHT,
            "frame_rate": FRAME_RATE,
            "transparent_background": True,
        },
    )
    rundown = stream_builder.add(LowerThirdRundown)
    station_bug = stream_builder.add(
        RiveSource,
        config={
            "riv_path": STATION_BUG_RIV_PATH,
            "width": STATION_BUG_SIZE,
            "height": STATION_BUG_SIZE,
            "frame_rate": FRAME_RATE,
        },
    )
    compositor = stream_builder.add(
        AlphaOverCompositor,
        config={
            "width": CANVAS_WIDTH,
            "height": CANVAS_HEIGHT,
            "layers": [
                {},
                {"x": SAFE_AREA_MARGIN, "y": CANVAS_HEIGHT - LOWER_THIRD_HEIGHT - SAFE_AREA_MARGIN},
                {"x": CANVAS_WIDTH - STATION_BUG_SIZE - SAFE_AREA_MARGIN, "y": SAFE_AREA_MARGIN},
            ],
        },
    )
    encoder = stream_builder.add(H264Encoder)

    stream_builder.connect(rundown.output("lower_third_to_page"), lower_third_page.input("data"))
    stream_builder.connect(camera.output("video"), compositor.input("layer_0"))
    stream_builder.connect(lower_third_page.output("video"), compositor.input("layer_1"))
    stream_builder.connect(station_bug.output("video"), compositor.input("layer_2"))
    stream_builder.connect(compositor.output("video"), encoder.input("video"))
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PRIVATE)
