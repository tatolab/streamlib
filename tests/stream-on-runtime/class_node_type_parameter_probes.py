# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A class node whose output is typed by its type parameter, and the reader past it.

`HandsOnEachPicture` states no format on its output: the output carries the type linked into
its input, so the pictures the test pattern renders reach the reader as they left it.
"""

import json
from typing import Annotated

from tatolab.stream import InputPin, OutputPin, StreamDelivery, Texture, log, node

RESULT_MARKER = "MARKER:PICTURES_HANDED_ON "
PICTURES_REPORTED = 2


@node
class HandsOnEachPicture[Frame: Texture]:
    """Hands on each picture it reads, unchanged."""

    picture_in: InputPin[Annotated[Frame, StreamDelivery("ordered")]]
    picture_out: OutputPin[Frame]

    def process(self, ctx) -> None:
        frame = self.picture_in.read()
        if frame is not None:
            self.picture_out.write(frame)


@node
class ReportsThePicturesItReads:
    """Reports the first pictures it reads."""

    pictures: InputPin[Annotated[Texture, StreamDelivery("ordered")]]

    def __init__(self) -> None:
        self._seen = []

    def process(self, ctx) -> None:
        if len(self._seen) >= PICTURES_REPORTED:
            return
        frame = self.pictures.read()
        if frame is None:
            return
        self._seen.append(
            {
                "surface_id": frame.surface_id,
                "width": frame.width,
                "height": frame.height,
                "format": frame.format,
                "timestamp_ns": frame.timestamp_ns,
            }
        )
        if len(self._seen) == PICTURES_REPORTED:
            log.info(RESULT_MARKER + json.dumps(self._seen))
