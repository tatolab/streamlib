# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Rive lower third: `tatolab.rive.RiveSource` → H.264, exposed on this machine, its text driven by data.

Point `RIV_PATH` at a `.riv` file whose artboard's default view-model instance
has a string property at `TITLE_PROPERTY_PATH`. Once a second the ticker sends a
new title on the source's `data` input. Watch it at
http://127.0.0.1:4446/watch/<machine>/main.
"""

from tatolab.stream import (
    Exposure,
    H264Encoder,
    StreamBuilder,
    stream,
)

from lower_third_title_ticker import LowerThirdTitleTicker
from tatolab.rive import RiveSource

RIV_PATH = "lower_third.riv"
TITLE_PROPERTY_PATH = "Name"
ENTRANCE_TRIGGER_PATH = "IN"


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The lower third over transparency at 1080p60, encoded and exposed."""
    rive = stream_builder.add(
        RiveSource,
        config={"riv_path": RIV_PATH, "width": 1920, "height": 1080, "frame_rate": 60},
    )
    ticker = stream_builder.add(
        LowerThirdTitleTicker,
        config={
            "property_path": TITLE_PROPERTY_PATH,
            "entrance_trigger_path": ENTRANCE_TRIGGER_PATH,
        },
    )
    encoder = stream_builder.add(H264Encoder)
    stream_builder.connect(ticker.output("data"), rive.input("data"))
    stream_builder.connect(rive.output("video"), encoder.input("video"))
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PRIVATE)
