# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A page mixing the stream's camera and microphone: both wired into a `BrowserSource`, with a probe.

`page_mixing_inputs.html` draws `video_in_0` into a canvas and plays
`audio_in_0` through WebAudio with a level meter, reporting each chunk's
timestamp, so the probe's reports in the project's log carry each input's
latency from capture through the page. Watch it at
http://127.0.0.1:4446/watch/<machine>/main.
"""

from pathlib import Path

from tatolab.camera import CameraSource
from tatolab.h264 import H264Encoder
from tatolab.microphone import MicrophoneSource
from tatolab.stream import Exposure, StreamBuilder, stream

from tatolab.browser import BrowserSource, BrowserSourceOutputProbe

PAGE_MIXING_INPUTS_URL = (Path(__file__).resolve().parent / "page_mixing_inputs.html").as_uri()

CANVAS_WIDTH = 1920
CANVAS_HEIGHT = 1080
FRAME_RATE = 30


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The camera and microphone into the page at 1080p30, encoded and exposed; measured."""
    camera = stream_builder.add(
        CameraSource, config={"max_width": CANVAS_WIDTH, "max_height": CANVAS_HEIGHT}
    )
    microphone = stream_builder.add(MicrophoneSource)
    browser = stream_builder.add(
        BrowserSource,
        config={
            "url": PAGE_MIXING_INPUTS_URL,
            "width": CANVAS_WIDTH,
            "height": CANVAS_HEIGHT,
            "frame_rate": FRAME_RATE,
            "transparent_background": False,
        },
    )
    probe = stream_builder.add(BrowserSourceOutputProbe, config={"frames_between_reports": 300})
    encoder = stream_builder.add(H264Encoder)
    stream_builder.connect(camera.output("video"), browser.input("video_in_0"))
    stream_builder.connect(microphone.output("audio"), browser.input("audio_in_0"))
    stream_builder.connect(browser.output("video"), probe.input("video_from_browser"))
    stream_builder.connect(browser.output("audio"), probe.input("audio_from_browser"))
    stream_builder.connect(browser.output("events"), probe.input("events_from_browser"))
    stream_builder.connect(probe.output("data_to_browser"), browser.input("data"))
    stream_builder.connect(browser.output("video"), encoder.input("video"))
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PRIVATE)
