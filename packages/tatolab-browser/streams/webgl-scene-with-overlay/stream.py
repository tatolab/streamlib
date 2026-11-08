# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A raymarched WebGL scene at 1080p60 with a transparent HTML lower third composited over it, seen in a viewer.

Both pages are tabs in the machine's one browser host. `scene_page.html` draws
a full-screen fragment shader every frame; `overlay_page.html` paints a lower
third over transparency, which the compositor lays over the scene. Watch the
compositor's `video` in Tato Studio's viewer; the probe on the scene reports
its frame rate in the project's log.
"""

from pathlib import Path

from tatolab.compositor import AlphaOverCompositor
from tatolab.stream import StreamBuilder, stream

from tatolab.browser import BrowserSource, BrowserSourceOutputProbe

PAGES_DIRECTORY = Path(__file__).resolve().parent
SCENE_PAGE_URL = (PAGES_DIRECTORY / "scene_page.html").as_uri()
OVERLAY_PAGE_URL = (PAGES_DIRECTORY / "overlay_page.html").as_uri()

CANVAS_WIDTH = 1920
CANVAS_HEIGHT = 1080
OVERLAY_WIDTH = 1760
OVERLAY_HEIGHT = 240


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The scene on `layer_0`, the lower third on `layer_1`."""
    scene = stream_builder.add(
        BrowserSource,
        config={"url": SCENE_PAGE_URL, "width": CANVAS_WIDTH, "height": CANVAS_HEIGHT, "frame_rate": 60},
    )
    overlay = stream_builder.add(
        BrowserSource,
        config={
            "url": OVERLAY_PAGE_URL,
            "width": OVERLAY_WIDTH,
            "height": OVERLAY_HEIGHT,
            "frame_rate": 60,
            "transparent_background": True,
        },
    )
    compositor = stream_builder.add(
        AlphaOverCompositor,
        config={
            "width": CANVAS_WIDTH,
            "height": CANVAS_HEIGHT,
            "layers": [{}, {"x": 80, "y": CANVAS_HEIGHT - OVERLAY_HEIGHT - 40}],
        },
    )
    stream_builder.connect(scene.output("video"), compositor.input("layer_0"))
    stream_builder.connect(overlay.output("video"), compositor.input("layer_1"))

    probe = stream_builder.add(BrowserSourceOutputProbe)
    stream_builder.connect(scene.output("video"), probe.input("video_from_browser"))
    stream_builder.connect(scene.output("audio"), probe.input("audio_from_browser"))
    stream_builder.connect(scene.output("events"), probe.input("events_from_browser"))
    stream_builder.connect(probe.output("data_to_browser"), scene.input("data"))
