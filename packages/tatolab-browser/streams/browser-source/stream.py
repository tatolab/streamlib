# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A web page as a source: `tatolab.browser.BrowserSource`, seen in a viewer, with a probe.

`probe_page.html` animates every frame, plays a 440 Hz tone and answers each
`data` message from its next animation frame, so the probe's reports in the
project's log carry the frame rate, the audio's continuity and the `data`
round trip. Watch the browser's `video` in Tato Studio's viewer.
"""

from pathlib import Path

from tatolab.stream import StreamBuilder, stream

from tatolab.browser import BrowserSource, BrowserSourceOutputProbe

PROBE_PAGE_URL = (Path(__file__).resolve().parent / "probe_page.html").as_uri()


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The probe page at 1080p60, its outputs measured."""
    browser = stream_builder.add(
        BrowserSource,
        config={"url": PROBE_PAGE_URL, "width": 1920, "height": 1080, "frame_rate": 60},
    )
    probe = stream_builder.add(BrowserSourceOutputProbe)
    stream_builder.connect(browser.output("video"), probe.input("video_from_browser"))
    stream_builder.connect(browser.output("audio"), probe.input("audio_from_browser"))
    stream_builder.connect(browser.output("events"), probe.input("events_from_browser"))
    stream_builder.connect(probe.output("data_to_browser"), browser.input("data"))
